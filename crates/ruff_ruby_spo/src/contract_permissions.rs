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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use lib_ruby_parser::Node;
use lib_ruby_parser::nodes::Send as SendNode;
use lib_ruby_parser::traverse::visitor::{Visitor, visit_send};

use crate::guards::{
    ClassEntry, Lowerer, Mixin, MixinArg, Parsed, Scan, collect_rb, const_path, flat_source,
    is_absolute, line_of, parse, snippet_of,
};

/// The name of the class `collect_ancestor_attributes` stops in front of.
const ROOT_CONTRACT: &str = "BaseContract";

/// The key `default_attribute_permission` writes (base_contract.rb:92-94).
/// `attribute_permission :default_permission, …` writes the same key.
const DEFAULT_KEY: &str = "default_permission";

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
    /// Keys written both as a symbol and as a string somewhere in the chain.
    /// `with_indifferent_access` (`base_contract.rb:184, 210`) then keeps the
    /// value of whichever key comes last in the merged hash, which depends on
    /// first-insertion positions, not on the last write; lookups of these
    /// keys are unresolved.
    pub mixed_spelling: BTreeSet<String>,
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
        if self.mixed_spelling.contains(canonical) {
            return mixed(canonical);
        }
        // The default lives under `:default_permission`, which a lookup of
        // that very name finds first (base_contract.rb:253).
        if canonical == DEFAULT_KEY
            && let Some(default) = &self.default
        {
            if let Some(gap) = self.gaps.iter().find(|g| g.order > default.order) {
                return PermissionState::Unresolved {
                    reason: format!(
                        "a later unreadable declaration may override: {}",
                        gap.reason
                    ),
                };
            }
            return default_state(default);
        }
        let (found, via_id) = match self.permissions.get(canonical) {
            Some(decl) => (Some(decl), false),
            None if self.mixed_spelling.contains(&with_id) => return mixed(&with_id),
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
        if self.mixed_spelling.contains(DEFAULT_KEY) {
            return mixed(DEFAULT_KEY);
        }
        match &self.default {
            Some(default) => default_state(default),
            None => PermissionState::ProvenAbsent,
        }
    }
}

fn default_state(default: &DefaultPermissionDecl) -> PermissionState<'_> {
    match &default.value {
        PermissionValue::AnyOf(_) => PermissionState::Default(default),
        PermissionValue::Unlowered {
            reason,
            source_text,
        } => PermissionState::Unresolved {
            reason: format!("default permission {reason}: {source_text}"),
        },
    }
}

fn mixed(key: &str) -> PermissionState<'static> {
    PermissionState::Unresolved {
        reason: format!(
            "`{key}` is written both as a symbol and as a string; `with_indifferent_access` keeps the entry that sits last in the merged hash"
        ),
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
        /// Written as a string (`"a"`), not a symbol.
        string_key: bool,
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
    /// An `include` / `prepend` the scan does not expand (inside a
    /// conditional or loop, on an explicit receiver, a `prepend`). It is a gap
    /// and it may also put modules into the ancestry, which turns a later
    /// `include` of one of them into a no-op. `modules` holds the constant
    /// paths as written; empty means unknown (any module).
    MaybeInstall {
        modules: Vec<String>,
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
    if ctx == Ctx::ModuleBody {
        if let Node::Send(send) = stmt
            && receiverless(send)
            && send.method_name == "extend"
            && send
                .args
                .iter()
                .any(|arg| const_path(arg).as_deref() == Some("ActiveSupport::Concern"))
        {
            entry.is_concern = true;
        }
        // Singleton methods Ruby calls on `include`; they are applied at the
        // include point (see `State::module`).
        if let Node::Defs(def) = stmt
            && is_include_hook(&def.name)
        {
            entry
                .include_hooks
                .push((def.name.clone(), src_ref(file, src, scope, stmt)));
            return;
        }
        if let Node::SClass(sclass) = stmt
            && matches!(&*sclass.expr, Node::Self_(_))
        {
            let body: Vec<&Node> = match sclass.body.as_deref() {
                Some(Node::Begin(b)) => b.statements.iter().collect(),
                Some(other) => vec![other],
                None => Vec::new(),
            };
            for inner in body {
                if let Node::Def(def) = inner
                    && is_include_hook(&def.name)
                {
                    entry
                        .include_hooks
                        .push((def.name.clone(), src_ref(file, src, scope, inner)));
                }
            }
        }
    }
    let mut out = Vec::new();
    classify(stmt, file, src, scope, ctx, &mut out);
    entry.events.extend(out);
}

/// `Module#include` calls `append_features` and then `included` on the
/// module; a module that defines either decides what including it does.
/// (`prepended` / `prepend_features` run only on `prepend`, which is never
/// expanded.)
fn is_include_hook(name: &str) -> bool {
    matches!(name, "included" | "append_features")
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
                Some(path) => MixinArg::Const {
                    path,
                    absolute: is_absolute(arg),
                },
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
            } else if send.recv.is_some()
                || ctx == Ctx::IncludedBody
                || send.method_name == "prepend"
            {
                // The scan expands only a receiverless `include` of a class or
                // module body (and of an `included` block, handled above). A
                // `prepend` runs `prepend_features` / `prepended` hooks and a
                // Concern's `prepended` block, none of which is read.
                out.push(ContractEvent::MaybeInstall {
                    modules: mixin_paths(&send.args),
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
    for (name, line, mixin) in finder.found {
        let reason = format!(
            "`{name}` is called inside a conditional, loop or other construct this arm does not unwrap"
        );
        let at = SrcRef {
            file,
            scope: scope.to_string(),
            line,
        };
        out.push(match mixin {
            Some(modules) => ContractEvent::MaybeInstall {
                modules,
                reason,
                at,
            },
            None => ContractEvent::Gap { reason, at },
        });
    }
}

/// Constant paths of `include` / `prepend` arguments, as written; empty when
/// any argument is not a constant path (the module is then unknown).
fn mixin_paths(args: &[Node]) -> Vec<String> {
    let mut paths = Vec::new();
    for arg in args {
        match const_path(arg) {
            Some(path) => paths.push(path),
            None => return Vec::new(),
        }
    }
    paths
}

struct DslFinder<'s> {
    src: &'s str,
    /// DSL call or mixin call, its line, and for a mixin call the modules it
    /// may install ([`mixin_paths`]).
    found: Vec<(String, usize, Option<Vec<String>>)>,
}

impl Visitor for DslFinder<'_> {
    fn on_send(&mut self, node: &SendNode) {
        let name = node.method_name.as_str();
        // A hidden `include` / `prepend` can install a module whose hooks
        // declare permissions.
        if receiverless(node)
            && ((DSL.contains(&name) && can_carry_permission(node))
                || matches!(name, "include" | "prepend"))
        {
            let mixin = matches!(name, "include" | "prepend").then(|| mixin_paths(&node.args));
            self.found.push((
                node.method_name.clone(),
                line_of(self.src, node.expression_l.begin),
                mixin,
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
        [_, .., Node::Kwargs(k)] => k.pairs.iter().any(|p| option_key(p) != OptionKey::Other),
        [_, .., Node::Hash(h)] => h.pairs.iter().any(|p| option_key(p) != OptionKey::Other),
        _ => true,
    }
}

/// What one entry of an `attribute` options hash is, for `options[:permission]`
/// (`base_contract.rb:87`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionKey {
    /// The literal key `:permission`.
    Permission,
    /// A literal key that is not `:permission`. A string key (even
    /// `"permission"`) is never read: the options hash is a plain `Hash` and
    /// is indexed with the symbol.
    Other,
    /// A `**` splat or a computed key: it may be `:permission`.
    Dynamic,
}

fn option_key(pair: &Node) -> OptionKey {
    match pair {
        Node::Pair(p) => match &*p.key {
            Node::Sym(k) if k.name.to_string_lossy() == "permission" => OptionKey::Permission,
            Node::Sym(_) | Node::Str(_) => OptionKey::Other,
            _ => OptionKey::Dynamic,
        },
        _ => OptionKey::Dynamic,
    }
}

/// Why a [`OptionKey::Dynamic`] entry may be `permission:`.
fn dynamic_option_reason(src: &str, pair: &Node, attribute: &str) -> String {
    match pair {
        Node::Pair(p) => format!(
            "computed option key `{}` in `attribute {attribute}` may be `:permission`",
            snippet_of(src, &p.key)
        ),
        _ => format!("`**` splat in `attribute {attribute}` may carry `permission:`"),
    }
}

/// A literal attribute name and whether it is a string. `"a"` and `:a` are
/// the same key at lookup (`with_indifferent_access`,
/// `base_contract.rb:184, 210`).
fn literal_name(node: Option<&Node>) -> Option<(String, bool)> {
    match node {
        Some(Node::Sym(s)) => Some((s.name.to_string_lossy(), false)),
        Some(Node::Str(s)) => Some((s.value.to_string_lossy(), true)),
        _ => None,
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
            let name = literal_name(send.args.first());
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
            let found = pairs
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, pair)| match pair {
                    Node::Pair(p) if option_key(pair) == OptionKey::Permission => {
                        Some((i, &*p.value))
                    }
                    _ => None,
                });
            let dynamic_after = |from: usize| {
                pairs[from..]
                    .iter()
                    .find(|pair| option_key(pair) == OptionKey::Dynamic)
            };
            let Some((name, string_key)) = name else {
                if found.is_some() || dynamic_after(0).is_some() {
                    out.push(gap(format!(
                        "`attribute` with a non-literal name may carry a permission: {}",
                        flat_source(src, whole)
                    )));
                }
                return;
            };
            let written = if string_key {
                format!("\"{name}\"")
            } else {
                format!(":{name}")
            };
            match found {
                None => {
                    if let Some(pair) = dynamic_after(0) {
                        out.push(gap(dynamic_option_reason(src, pair, &written)));
                    }
                }
                Some((i, value)) => {
                    if let Some(pair) = dynamic_after(i + 1) {
                        let reason = match pair {
                            Node::Pair(_) => {
                                "a computed option key after `permission:` can override it"
                            }
                            _ => "a `**` splat after `permission:` can override it",
                        };
                        out.push(ContractEvent::Permission {
                            attribute: name,
                            string_key,
                            value: PermissionValue::Unlowered {
                                reason: reason.to_string(),
                                source_text: flat_source(src, value),
                            },
                            form: DeclForm::AttributeOption,
                            at,
                        });
                    } else if let Lowered::Value(value) = lower_permission(value, src) {
                        out.push(ContractEvent::Permission {
                            attribute: name,
                            string_key,
                            value,
                            form: DeclForm::AttributeOption,
                            at,
                        });
                    }
                }
            }
        }
        "attribute_permission" => match (send.args.as_slice(), literal_name(send.args.first())) {
            ([_, value], Some((name, string_key))) => {
                if let Lowered::Value(value) = lower_permission(value, src) {
                    out.push(ContractEvent::Permission {
                        attribute: name,
                        string_key,
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
    scan.record_permissions = true;
    for (index, file) in files.iter().enumerate() {
        if let Some(ast) = &file.ast {
            scan.walk(index, &file.src, ast, &[]);
        }
    }
    // A file that did not parse could reopen any class or module.
    let unparsed: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.parse_failed)
        .map(|(i, _)| i)
        .collect();
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
        .map(|name| resolve_class(&lowerer, name, &unparsed))
        .collect()
}

struct State<'l, 'a> {
    lowerer: &'l Lowerer<'a>,
    out: ContractPermissions,
    order: usize,
    /// Modules already in the ancestry of the class being resolved, across
    /// its superclass chain. Including one again installs nothing:
    /// `ActiveSupport::Concern#append_features` returns early when
    /// `base < self` (so neither its dependencies nor its `included` block run
    /// again), and `Module#append_features` skips a module that is already an
    /// ancestor. `Module#include` still calls the module's `included` hook.
    /// Kept apart from the recursion `stack`, which only breaks cycles.
    installed: HashSet<String>,
    /// Modules that an unexpanded `include` / `prepend` may have put into the
    /// ancestry (constant paths as written).
    possibly_installed: Vec<String>,
    /// Something the scan could not read may have put any module into the
    /// ancestry (a dynamic or unscanned include, an include hook).
    ancestry_unknown: bool,
    /// A library module the scan cannot read was included. It may install
    /// other library modules, but a library cannot name an application
    /// constant, so it cannot install a module of the scanned tree.
    library_ancestry_unknown: bool,
    /// Files that did not parse and reopen the root contract: a module they
    /// name may already be an ancestor.
    root_unparsed: Vec<usize>,
    /// Per key: written as a symbol, written as a string.
    spellings: BTreeMap<String, (bool, bool)>,
    /// Every scanned class and module whose body contributed.
    touched: BTreeSet<String>,
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

    fn spelled(&mut self, key: &str, string_key: bool) {
        let entry = self.spellings.entry(key.to_string()).or_default();
        if string_key {
            entry.1 = true;
        } else {
            entry.0 = true;
        }
    }

    fn apply(&mut self, event: &ContractEvent, applied_in: &str) {
        match event {
            ContractEvent::Permission {
                attribute,
                string_key,
                value,
                form,
                at,
            } => {
                self.spelled(attribute, *string_key);
                let source = self.source(at);
                let order = self.tick();
                if attribute == DEFAULT_KEY {
                    // Same hash entry as `default_attribute_permission`
                    // (base_contract.rb:93): the later write wins.
                    self.out.default = Some(DefaultPermissionDecl {
                        value: value.clone(),
                        source,
                        applied_in: applied_in.to_string(),
                        order,
                    });
                    return;
                }
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
                self.spelled(DEFAULT_KEY, false);
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
            ContractEvent::MaybeInstall {
                modules,
                reason,
                at,
            } => {
                self.gap(reason.clone(), at);
                self.mark_possibly_installed(modules);
            }
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
                MixinArg::Const {
                    path: name,
                    absolute,
                } => match self.lowerer.resolve_arg(&mixin.ns, name, *absolute) {
                    Some(found) => self.module(&found, at, applied_in, stack),
                    None => {
                        self.unscanned_installed(name);
                        self.gap(
                            format!(
                                "module `{name}` is not scanned; an `included` hook could declare permissions"
                            ),
                            at,
                        );
                    }
                },
                MixinArg::Dynamic(what) => {
                    self.ancestry_unknown = true;
                    self.gap(what.clone(), at);
                }
            }
        }
    }

    /// Include module `name` into `applied_in`; `at` is the `include` call.
    fn module(&mut self, name: &str, at: &SrcRef, applied_in: &str, stack: &mut Vec<String>) {
        if stack.iter().any(|n| n == name) {
            return;
        }
        let Some(entry) = self.lowerer.classes.get(name) else {
            return;
        };
        self.touched.insert(name.to_string());
        let hook = |wanted: &str| entry.include_hooks.iter().find(|(h, _)| h == wanted);
        let replaced = hook("append_features");
        let included = hook("included");
        if self.installed.contains(name) {
            // Already an ancestor: nothing is installed again, but a
            // singleton `append_features` or `included` this module defines
            // is still called.
            if let Some((hook_name, hook_at)) = replaced.or(included) {
                let def = self.source(hook_at);
                self.gap(
                    format!(
                        "module `{name}` is already included, but its `self.{hook_name}` ({}:{}) runs again",
                        def.file, def.line
                    ),
                    at,
                );
            }
            return;
        }
        // An unexpanded include may already have installed this module, in
        // which case this include is a no-op. Both outcomes stay open: the
        // replay is applied, then a gap follows it.
        let maybe_noop = self.may_be_installed(name);
        stack.push(name.to_string());
        // Concern dependencies first, then the `included` block. An own
        // `append_features` replaces the code that installs both.
        for event in &entry.events {
            match event {
                ContractEvent::Include { mixin, at } => {
                    if replaced.is_none() {
                        self.include(&entry.mixins[*mixin], at, applied_in, stack);
                    }
                }
                other => self.apply(other, applied_in),
            }
        }
        if let Some((_, hook_at)) = replaced {
            self.ancestry_unknown = true;
            let def = self.source(hook_at);
            self.gap(
                format!(
                    "module `{name}` defines `self.append_features` ({}:{}), which decides what including it does",
                    def.file, def.line
                ),
                at,
            );
        } else if let Some((_, hook_at)) = included {
            // The hook runs after `append_features`, so it can override the
            // block; written before `included do`, it also keeps the block
            // from being stored. Neither is read: one gap stands for both.
            self.installed.insert(name.to_string());
            self.ancestry_unknown = true;
            let def = self.source(hook_at);
            self.gap(
                format!(
                    "module `{name}` defines `self.included` ({}:{}), which can declare attributes and decides whether its `included` block runs",
                    def.file, def.line
                ),
                at,
            );
        } else if !entry.is_concern && !entry.included_events.is_empty() {
            // `included do … end` stores the block only through
            // `ActiveSupport::Concern#included`; without `extend
            // ActiveSupport::Concern` the call reaches some other `included`.
            self.installed.insert(name.to_string());
            self.ancestry_unknown = true;
            self.gap(
                format!(
                    "module `{name}` has an `included do` block but does not `extend ActiveSupport::Concern`; whether the block runs is not known"
                ),
                at,
            );
        } else {
            self.installed.insert(name.to_string());
            for event in &entry.included_events {
                match event {
                    ContractEvent::IncludeInIncluder { mixin, at } => {
                        self.include(&entry.included_mixins[*mixin], at, applied_in, stack);
                    }
                    other => self.apply(other, applied_in),
                }
            }
        }
        if maybe_noop {
            self.gap(
                format!(
                    "this include of `{name}` may be a no-op: something the scan could not expand may already have included it"
                ),
                at,
            );
        }
        stack.pop();
    }

    /// An unscanned module was included. One in a namespace the scan defines
    /// nothing under is library code; anything else may be application code
    /// and may install any module.
    fn unscanned_installed(&mut self, path: &str) {
        if self.is_application_namespace(path) {
            self.ancestry_unknown = true;
        } else {
            self.library_ancestry_unknown = true;
        }
    }

    /// Whether the outermost segment of `path` names a class or module that
    /// the scan defines, or a namespace one of them lives under.
    fn is_application_namespace(&self, path: &str) -> bool {
        let first = path
            .trim_start_matches("::")
            .split("::")
            .next()
            .unwrap_or("");
        self.lowerer
            .classes
            .keys()
            .any(|name| name.split("::").next() == Some(first))
    }

    fn mark_possibly_installed(&mut self, modules: &[String]) {
        if modules.is_empty() {
            self.ancestry_unknown = true;
        } else {
            self.possibly_installed.extend(modules.iter().cloned());
        }
    }

    /// Whether `name` (fully qualified) may already be an ancestor although
    /// the scan did not see it installed.
    fn may_be_installed(&self, name: &str) -> bool {
        if self.ancestry_unknown
            || (self.library_ancestry_unknown && !self.is_application_namespace(name))
        {
            return true;
        }
        let written_as = |path: &String| {
            let path = path.trim_start_matches("::");
            name == path || name.ends_with(&format!("::{path}"))
        };
        if self.possibly_installed.iter().any(written_as) {
            return true;
        }
        self.root_unparsed.iter().any(|&file| {
            self.lowerer.files.get(file).is_some_and(|parsed| {
                name.split("::")
                    .all(|segment| mentions_word(&parsed.src, segment))
            })
        })
    }

    /// Record what the root contract puts into the ancestry, without applying
    /// its declarations (they are excluded, `base_contract.rb:203-205`): a
    /// module the root includes is already an ancestor of every subclass.
    fn seed_root_ancestry(&mut self, name: &str) {
        let Some(entry) = self.lowerer.classes.get(name) else {
            return;
        };
        for event in &entry.events {
            match event {
                ContractEvent::Include { mixin, .. } => self.seed_mixin(&entry.mixins[*mixin]),
                ContractEvent::MaybeInstall { modules, .. } => {
                    self.mark_possibly_installed(modules);
                }
                _ => {}
            }
        }
    }

    fn seed_mixin(&mut self, mixin: &Mixin) {
        for arg in &mixin.names {
            match arg {
                MixinArg::Const { path, absolute } => {
                    match self.lowerer.resolve_arg(&mixin.ns, path, *absolute) {
                        Some(found) => self.seed_module(&found),
                        None => self.unscanned_installed(path),
                    }
                }
                MixinArg::Dynamic(_) => self.ancestry_unknown = true,
            }
        }
    }

    fn seed_module(&mut self, name: &str) {
        if !self.installed.insert(name.to_string()) {
            return;
        }
        let Some(entry) = self.lowerer.classes.get(name) else {
            return;
        };
        if !entry.include_hooks.is_empty() {
            self.ancestry_unknown = true;
        }
        for event in &entry.events {
            match event {
                ContractEvent::Include { mixin, .. } => self.seed_mixin(&entry.mixins[*mixin]),
                ContractEvent::MaybeInstall { modules, .. } => {
                    self.mark_possibly_installed(modules);
                }
                _ => {}
            }
        }
        for event in &entry.included_events {
            match event {
                ContractEvent::IncludeInIncluder { mixin, .. } => {
                    self.seed_mixin(&entry.included_mixins[*mixin]);
                }
                ContractEvent::MaybeInstall { modules, .. } => {
                    self.mark_possibly_installed(modules);
                }
                _ => {}
            }
        }
    }
}

fn resolve_class(lowerer: &Lowerer<'_>, class: &str, unparsed: &[usize]) -> ContractPermissions {
    // The superclass chain, subclass first, stopping before `BaseContract`
    // (base_contract.rb:203-205).
    let mut chain = vec![class.to_string()];
    let mut seen: HashSet<String> = HashSet::from([class.to_string()]);
    let mut unscanned: Option<(String, String)> = None;
    let mut current = class.to_string();
    let mut scanned_root = false;
    while let Some(entry) = lowerer.classes.get(&current) {
        if let Some(text) = &entry.dynamic_superclass {
            unscanned = Some((
                current.clone(),
                format!("superclass `{text}` is not a constant path; it may declare permissions"),
            ));
            break;
        }
        let Some((ns, sup)) = entry.superclass.clone() else {
            break;
        };
        match lowerer.resolve(&ns, &sup) {
            Some(found) if found == ROOT_CONTRACT => {
                scanned_root = true;
                break;
            }
            Some(found) => {
                if !seen.insert(found.clone()) {
                    break;
                }
                chain.push(found.clone());
                current = found;
            }
            None => {
                if sup != ROOT_CONTRACT {
                    unscanned = Some((
                        current.clone(),
                        format!("superclass `{sup}` is not scanned; it may declare permissions"),
                    ));
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
            mixed_spelling: BTreeSet::new(),
        },
        order: 0,
        installed: HashSet::new(),
        possibly_installed: Vec::new(),
        ancestry_unknown: false,
        library_ancestry_unknown: false,
        root_unparsed: Vec::new(),
        spellings: BTreeMap::new(),
        touched: chain.iter().cloned().collect(),
    };
    if let Some((from, reason)) = unscanned {
        // Root-most position: applied before everything else.
        // No file or line: the text lives outside the scan (`file` is empty).
        let at = SrcRef {
            file: usize::MAX,
            scope: from,
            line: 0,
        };
        state.gap(reason, &at);
    }
    // The root's own ancestry. An unscanned root is assumed to install
    // nothing a subclass includes again, as its declarations are excluded.
    if scanned_root {
        state.seed_root_ancestry(ROOT_CONTRACT);
        state.root_unparsed = unparsed
            .iter()
            .copied()
            .filter(|&file| {
                lowerer
                    .files
                    .get(file)
                    .is_some_and(|parsed| parsed.src.contains(&format!("class {ROOT_CONTRACT}")))
            })
            .collect();
    }
    // Root-most first, so a subclass key overwrites: `reverse_merge!` semantics.
    for name in chain.iter().rev() {
        state.apply_class(name);
    }
    unparsed_gaps(&mut state, unparsed);
    state.out.mixed_spelling = state
        .spellings
        .iter()
        .filter(|(_, (sym, string))| *sym && *string)
        .map(|(key, _)| key.clone())
        .collect();
    state.out
}

/// A file that did not parse may reopen a class or module of the chain. It
/// can only do that by naming it, so it counts when its text mentions every
/// segment of one contributing name (or when its text could not be read).
///
/// The reopening's position in the merge is unknown, so the gap takes the
/// last position: it can override every declaration that was read, so no
/// lookup of that class is proven. A file that names nothing of the chain is
/// not counted: a parsed file reaching a class without naming it
/// (`const_get`, a constant alias) is equally outside the scan.
fn unparsed_gaps(state: &mut State<'_, '_>, unparsed: &[usize]) {
    for &file in unparsed {
        let Some(parsed) = state.lowerer.files.get(file) else {
            continue;
        };
        let named = state.touched.iter().find(|name| {
            name.split("::")
                .all(|segment| mentions_word(&parsed.src, segment))
        });
        let reason = match (parsed.src.is_empty(), named) {
            (true, _) => format!(
                "`{}` could not be read; it may reopen any class",
                parsed.rel
            ),
            (false, Some(name)) => format!(
                "`{}` did not parse and names `{name}`; it may reopen it",
                parsed.rel
            ),
            (false, None) => continue,
        };
        let order = state.tick();
        state.out.gaps.push(ContractGap {
            reason,
            source: DeclSource {
                scope: state.out.class.clone(),
                file: parsed.rel.clone(),
                line: 0,
            },
            order,
        });
    }
}

/// `word` occurs in `src` with no identifier character on either side.
fn mentions_word(src: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    src.match_indices(word).any(|(at, _)| {
        !src[..at].chars().next_back().is_some_and(ident)
            && !src[at + word.len()..].chars().next().is_some_and(ident)
    })
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
        let mods = "module One\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :one\n  end\nend\nmodule Two\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :two\n  end\nend\n";
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
    fn unresolved(c: &ContractPermissions, attr: &str) -> String {
        match c.lookup(attr) {
            PermissionState::Unresolved { reason } => reason,
            other => panic!("{attr}: expected Unresolved, got {other:?}"),
        }
    }

    fn default_of<'a>(c: &'a ContractPermissions, attr: &str) -> &'a DefaultPermissionDecl {
        match c.lookup(attr) {
            PermissionState::Default(d) => d,
            other => panic!("{attr}: expected Default, got {other:?}"),
        }
    }

    const CONCERN_EXTRA: &str = "module Extra\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :mixin\n  end\nend\n";

    #[test]
    fn a_superclass_that_is_not_a_constant_path_is_a_root_most_gap() {
        let src = "class C < Object.const_get(:Parent)\n  attribute :a, permission: :x\nend\n";
        let all = scan("dyn_super", &[("c.rb", src)]);
        let c = class(&all, "C");
        // Root-most: the class's own declaration still wins over it ...
        assert_eq!(explicit(c, "a").value, any_of(&["x"]));
        // ... but anything else could come from the unknown superclass.
        let reason = unresolved(c, "other");
        assert!(reason.contains("Object.const_get(:Parent)"), "{reason}");
        assert_eq!(c.gaps.len(), 1);
        assert_eq!(c.gaps[0].order, 1);
    }

    #[test]
    fn an_omitted_or_resolved_root_superclass_is_not_a_gap() {
        let src = "class Omitted\n  attribute :a, permission: :x\nend\nclass Rooted < ::BaseContract\n  attribute :a, permission: :x\nend\n";
        let all = scan("dyn_super_twin", &[("c.rb", src)]);
        for name in ["Omitted", "Rooted"] {
            let c = class(&all, name);
            assert!(c.gaps.is_empty(), "{name}: {:?}", c.gaps);
            assert_eq!(c.lookup("other"), PermissionState::ProvenAbsent);
        }
    }

    #[test]
    fn an_own_self_included_keeps_the_included_block_uncertain() {
        let defs = "module Hooked\n  extend ActiveSupport::Concern\n  def self.included(base)\n    super\n  end\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let sclass = "module Hooked\n  extend ActiveSupport::Concern\n  class << self\n    def included(base)\n      super\n    end\n  end\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let class_src = "class C < BaseContract\n  include Hooked\nend\n";
        for (case, module) in [("hook_defs", defs), ("hook_sclass", sclass)] {
            let all = scan(case, &[("m.rb", module), ("c.rb", class_src)]);
            let reason = unresolved(class(&all, "C"), "a");
            assert!(reason.contains("`self.included`"), "{case}: {reason}");
        }
    }

    #[test]
    fn a_concern_without_a_hook_replays_its_included_block() {
        let module = "module Plain\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let all = scan(
            "hook_twin",
            &[
                ("m.rb", module),
                ("c.rb", "class C < BaseContract\n  include Plain\nend\n"),
            ],
        );
        let c = class(&all, "C");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        assert_eq!(explicit(c, "a").value, any_of(&["blk"]));
    }

    #[test]
    fn an_included_block_without_concern_is_a_gap() {
        let module =
            "module NotConcern\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let all = scan(
            "no_concern",
            &[
                ("m.rb", module),
                (
                    "c.rb",
                    "class C < BaseContract\n  include NotConcern\nend\n",
                ),
            ],
        );
        let reason = unresolved(class(&all, "C"), "a");
        assert!(reason.contains("ActiveSupport::Concern"), "{reason}");
    }

    #[test]
    fn an_own_append_features_is_a_gap_at_the_include() {
        let module = "module Replaced\n  extend ActiveSupport::Concern\n  def self.append_features(base)\n    super\n  end\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let all = scan(
            "append_features",
            &[
                ("m.rb", module),
                ("c.rb", "class C < BaseContract\n  include Replaced\nend\n"),
            ],
        );
        let c = class(&all, "C");
        let reason = unresolved(c, "a");
        assert!(reason.contains("`self.append_features`"), "{reason}");
        assert_eq!(c.gaps[0].source.file, "c.rb");
        assert_eq!(c.gaps[0].source.line, 2);
    }

    #[test]
    fn a_prepended_hook_does_not_run_on_include_but_prepend_is_a_gap() {
        // `Module#include` calls `append_features` and `included` only.
        let module = "module Pre\n  extend ActiveSupport::Concern\n  def self.prepended(base)\n    base.attribute :a, permission: :pre\n  end\n  included do\n    attribute :a, permission: :blk\n  end\nend\n";
        let all = scan(
            "prepended_include",
            &[
                ("m.rb", module),
                ("c.rb", "class C < BaseContract\n  include Pre\nend\n"),
            ],
        );
        let c = class(&all, "C");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        assert_eq!(explicit(c, "a").value, any_of(&["blk"]));
        let all = scan(
            "prepended_prepend",
            &[
                ("m.rb", module),
                ("c.rb", "class C < BaseContract\n  prepend Pre\nend\n"),
            ],
        );
        let reason = unresolved(class(&all, "C"), "a");
        assert!(reason.contains("prepend Pre"), "{reason}");
    }

    #[test]
    fn computed_option_keys_may_be_permission() {
        let src = "class Hash1 < BaseContract\n  attribute :a, { key => :view }\nend\n\
                   class After < BaseContract\n  attribute :secret, permission: :public, key => :admin\nend\n\
                   class Before < BaseContract\n  attribute :secret, key => :admin, permission: :public\nend\n\
                   class Literal < BaseContract\n  attribute :secret, permission: :public, writable: false, \"permission\" => :admin\nend\n";
        let all = scan("computed_keys", &[("c.rb", src)]);
        let reason = unresolved(class(&all, "Hash1"), "a");
        assert!(reason.contains("computed option key"), "{reason}");
        let reason = unresolved(class(&all, "After"), "secret");
        assert!(reason.contains("computed option key"), "{reason}");
        // A later literal `permission:` settles it.
        assert_eq!(
            explicit(class(&all, "Before"), "secret").value,
            any_of(&["public"])
        );
        // Literal keys, including the string "permission", are never read.
        let literal = class(&all, "Literal");
        assert!(literal.gaps.is_empty(), "{:?}", literal.gaps);
        assert_eq!(explicit(literal, "secret").value, any_of(&["public"]));
    }

    #[test]
    fn mixin_calls_the_scan_does_not_expand_are_gaps() {
        let cases = [
            (
                "mix_if",
                "class C < BaseContract\n  include Extra if flag?\nend\n",
            ),
            (
                "mix_loop",
                "class C < BaseContract\n  [Extra].each { |m| include m }\nend\n",
            ),
            (
                "mix_prepend",
                "class C < BaseContract\n  prepend Extra\nend\n",
            ),
        ];
        for (case, class_src) in cases {
            let all = scan(case, &[("m.rb", CONCERN_EXTRA), ("c.rb", class_src)]);
            let c = class(&all, "C");
            assert_eq!(c.gaps.len(), 1, "{case}: {:?}", c.gaps);
            assert_eq!(c.gaps[0].source.line, 2, "{case}");
            unresolved(c, "a");
        }
        // The direct include keeps its expansion: no gap.
        let all = scan(
            "mix_direct",
            &[
                ("m.rb", CONCERN_EXTRA),
                ("c.rb", "class C < BaseContract\n  include Extra\nend\n"),
            ],
        );
        let c = class(&all, "C");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        assert_eq!(explicit(c, "a").value, any_of(&["mixin"]));
    }

    #[test]
    fn default_permission_has_one_precedence_path() {
        let first = "class C < BaseContract\n  default_attribute_permission :edit\n  attribute_permission :default_permission, :manage\nend\n";
        let all = scan("default_one_a", &[("c.rb", first)]);
        let c = class(&all, "C");
        let d = default_of(c, "anything");
        assert_eq!(d.value, any_of(&["manage"]));
        assert_eq!(d.source.line, 3);
        assert_eq!(
            default_of(c, "default_permission").value,
            any_of(&["manage"])
        );
        assert!(c.permissions.is_empty());

        let second = "class C < BaseContract\n  attribute_permission :default_permission, :manage\n  default_attribute_permission :edit\nend\n";
        let all = scan("default_one_b", &[("c.rb", second)]);
        let d = default_of(class(&all, "C"), "anything");
        assert_eq!(d.value, any_of(&["edit"]));
        assert_eq!(d.source.line, 3);

        // Across the chain the subclass wins whichever form it uses.
        let chain = "class P < BaseContract\n  default_attribute_permission :edit\nend\nclass C < P\n  attribute :default_permission, permission: :manage\nend\n";
        let all = scan("default_one_chain", &[("c.rb", chain)]);
        let d = default_of(class(&all, "C"), "anything");
        assert_eq!(d.value, any_of(&["manage"]));
        assert_eq!(d.applied_in, "C");
    }

    #[test]
    fn unparsed_files_naming_the_class_keep_every_lookup_open() {
        let good = "class Widget < BaseContract\n  default_attribute_permission :d\n  attribute :a, permission: :x\nend\n";
        let bad = "class Widget < BaseContract\n  def (\nend\n";
        let all = scan("unparsed", &[("a.rb", good), ("bad.rb", bad)]);
        let c = class(&all, "Widget");
        for attr in ["a", "other"] {
            let reason = unresolved(c, attr);
            assert!(reason.contains("bad.rb"), "{attr}: {reason}");
        }
    }

    #[test]
    fn unparsed_files_naming_nothing_of_the_chain_change_nothing() {
        let good = "class Widget < BaseContract\n  default_attribute_permission :d\n  attribute :a, permission: :x\nend\n";
        let bad = "class Gadget\n  def (\nend\n";
        let all = scan("unparsed_twin", &[("a.rb", good), ("bad.rb", bad)]);
        let c = class(&all, "Widget");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        assert_eq!(explicit(c, "a").value, any_of(&["x"]));
        assert_eq!(default_of(c, "other").value, any_of(&["d"]));
    }

    #[test]
    fn string_keys_match_symbols_unless_both_spellings_are_written() {
        let src = "class S < BaseContract\n  attribute_permission \"a\", :edit\n  attribute \"b\", permission: :view\nend\n\
                   class M < BaseContract\n  attribute_permission :a, :one\n  attribute_permission \"a\", :two\nend\n\
                   class P < BaseContract\n  attribute_permission \"a\", :one\nend\nclass Q < P\n  attribute_permission :a, :two\nend\n";
        let all = scan("string_keys", &[("c.rb", src)]);
        let s = class(&all, "S");
        assert_eq!(explicit(s, "a").value, any_of(&["edit"]));
        assert_eq!(explicit(s, "a_id").value, any_of(&["edit"]));
        assert_eq!(explicit(s, "b").value, any_of(&["view"]));
        for name in ["M", "Q"] {
            let reason = unresolved(class(&all, name), "a");
            assert!(
                reason.contains("symbol and as a string"),
                "{name}: {reason}"
            );
        }
    }

    #[test]
    fn a_concern_already_in_the_ancestry_is_not_installed_again() {
        let same = "class C < BaseContract\n  include Extra\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let chain = "class P < BaseContract\n  include Extra\nend\nclass C < P\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        for (case, src) in [("idem_same", same), ("idem_chain", chain)] {
            let all = scan(case, &[("m.rb", CONCERN_EXTRA), ("c.rb", src)]);
            let c = class(&all, "C");
            assert!(c.gaps.is_empty(), "{case}: {:?}", c.gaps);
            assert_eq!(explicit(c, "a").value, any_of(&["mine"]), "{case}");
        }
    }

    #[test]
    fn a_different_module_or_a_hook_still_applies_after_an_include() {
        let other = "module Other\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :other\n  end\nend\n";
        let src = "class C < BaseContract\n  include Extra\n  attribute :a, permission: :mine\n  include Other\nend\n";
        let all = scan(
            "idem_other",
            &[("m.rb", CONCERN_EXTRA), ("o.rb", other), ("c.rb", src)],
        );
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["other"]));
        // A plain module's `self.included` runs on every `include`.
        let hooked = "module Hooked\n  def self.included(base)\n  end\nend\n";
        let src = "class C < BaseContract\n  include Hooked\n  attribute :a, permission: :mine\n  include Hooked\nend\n";
        let all = scan("idem_hook", &[("h.rb", hooked), ("c.rb", src)]);
        let reason = unresolved(class(&all, "C"), "a");
        assert!(reason.contains("runs again"), "{reason}");
    }

    #[test]
    fn an_include_after_an_unexpanded_include_of_the_same_module_stays_open() {
        // The final include is a no-op when `flag?` is true (`:mine` wins)
        // and replays the block when it is false (`:mixin` wins).
        let src = "class C < BaseContract\n  include Extra if flag?\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let all = scan("maybe_same", &[("m.rb", CONCERN_EXTRA), ("c.rb", src)]);
        let reason = unresolved(class(&all, "C"), "a");
        assert!(reason.contains("may be a no-op"), "{reason}");
        // A dynamic include may have installed any module.
        let src = "class C < BaseContract\n  include mods.first\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let all = scan("maybe_dyn", &[("m.rb", CONCERN_EXTRA), ("c.rb", src)]);
        assert!(unresolved(class(&all, "C"), "a").contains("may be a no-op"));
    }

    #[test]
    fn an_unexpanded_include_of_a_different_module_does_not_open_a_later_include() {
        let other = "module Other\n  extend ActiveSupport::Concern\nend\n";
        let src = "class C < BaseContract\n  include Other if flag?\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let all = scan(
            "maybe_other",
            &[("m.rb", CONCERN_EXTRA), ("o.rb", other), ("c.rb", src)],
        );
        let c = class(&all, "C");
        assert_eq!(explicit(c, "a").value, any_of(&["mixin"]));
        assert!(
            c.gaps.iter().all(|g| !g.reason.contains("may be a no-op")),
            "{:?}",
            c.gaps
        );
    }

    #[test]
    fn a_module_the_scanned_root_includes_is_not_installed_again() {
        let root = "class BaseContract\n  include Extra\nend\n";
        let src =
            "class C < BaseContract\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let all = scan(
            "root_seed",
            &[("m.rb", CONCERN_EXTRA), ("b.rb", root), ("c.rb", src)],
        );
        let c = class(&all, "C");
        assert!(c.gaps.is_empty(), "{:?}", c.gaps);
        assert_eq!(explicit(c, "a").value, any_of(&["mine"]));
        // Without the root's include, the block replays and wins.
        let root = "class BaseContract\nend\n";
        let all = scan(
            "root_seed_none",
            &[("m.rb", CONCERN_EXTRA), ("b.rb", root), ("c.rb", src)],
        );
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["mixin"]));
    }

    #[test]
    fn an_unparsed_root_naming_a_module_keeps_its_later_include_open() {
        // Ruby 3.2 anonymous argument forwarding: the parser reports an error.
        let src =
            "class C < BaseContract\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let naming =
            "class BaseContract\n  # include Extra\n  def validate(*)\n    valid?(*)\n  end\nend\n";
        let all = scan(
            "root_unparsed_naming",
            &[("m.rb", CONCERN_EXTRA), ("b.rb", naming), ("c.rb", src)],
        );
        assert!(unresolved(class(&all, "C"), "a").contains("may be a no-op"));
        let silent = "class BaseContract\n  def validate(*)\n    valid?(*)\n  end\nend\n";
        let all = scan(
            "root_unparsed_silent",
            &[("m.rb", CONCERN_EXTRA), ("b.rb", silent), ("c.rb", src)],
        );
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["mixin"]));
    }

    #[test]
    fn an_unscanned_library_module_cannot_install_an_application_module() {
        // `ActiveModel` is defined nowhere in the scan: library code.
        let root = "class BaseContract\n  include ActiveModel::Validations\nend\n";
        let src =
            "class C < BaseContract\n  attribute :a, permission: :mine\n  include Extra\nend\n";
        let all = scan(
            "lib_root",
            &[("m.rb", CONCERN_EXTRA), ("b.rb", root), ("c.rb", src)],
        );
        assert_eq!(explicit(class(&all, "C"), "a").value, any_of(&["mixin"]));
        // An unscanned module in an application namespace may be anything.
        let app = "module Extra\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :mixin\n  end\nend\nmodule Shared\nend\n";
        let root = "class BaseContract\n  include Shared::Missing\nend\n";
        let all = scan("app_root", &[("m.rb", app), ("b.rb", root), ("c.rb", src)]);
        assert!(unresolved(class(&all, "C"), "a").contains("may be a no-op"));
    }

    #[test]
    fn an_absolute_include_resolves_from_the_top_level() {
        let inner = "module N\n  module Extra\n    extend ActiveSupport::Concern\n    included do\n      attribute :a, permission: :inner\n    end\n  end\nend\n";
        let top = "module Extra\n  extend ActiveSupport::Concern\n  included do\n    attribute :a, permission: :top\n  end\nend\n";
        let absolute = "module N\n  class C < BaseContract\n    include ::Extra\n  end\nend\n";
        let relative = "module N\n  class C < BaseContract\n    include Extra\n  end\nend\n";
        let all = scan("abs", &[("i.rb", inner), ("t.rb", top), ("c.rb", absolute)]);
        assert_eq!(explicit(class(&all, "N::C"), "a").value, any_of(&["top"]));
        let all = scan(
            "abs_rel",
            &[("i.rb", inner), ("t.rb", top), ("c.rb", relative)],
        );
        assert_eq!(explicit(class(&all, "N::C"), "a").value, any_of(&["inner"]));
        // `::Extra` never falls back to `N::Extra`.
        let all = scan("abs_missing", &[("i.rb", inner), ("c.rb", absolute)]);
        let reason = unresolved(class(&all, "N::C"), "a");
        assert!(reason.contains("not scanned"), "{reason}");
    }

    #[test]
    fn guard_scans_do_not_record_permission_events() {
        let root =
            std::env::temp_dir().join(format!("ruff_ruby_spo_perms_{}_optin", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("c.rb");
        fs::write(
            &path,
            "class C < BaseContract\n  include Extra\n  attribute :a, permission: :x\nend\n",
        )
        .unwrap();
        let file = parse(&root, &path);
        let _ = fs::remove_dir_all(&root);
        let ast = file.ast.as_ref().unwrap();
        let mut guard = Scan::default();
        guard.walk(0, &file.src, ast, &[]);
        let entry = &guard.classes["C"];
        assert!(entry.events.is_empty());
        assert_eq!(entry.mixins.len(), 1);
        let mut perms = Scan::default();
        perms.record_permissions = true;
        perms.walk(0, &file.src, ast, &[]);
        assert_eq!(perms.classes["C"].events.len(), 2);
    }

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
        // Pinned baseline: the create contract and its chain read fully.
        assert!(c.gaps.is_empty(), "gaps: {:#?}", c.gaps);
        assert_eq!(NEXGEN_CREATE.len(), 29);
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
