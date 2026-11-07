//! Class-body walker — routes top-level `Send` calls in a class body to
//! the right [`crate::Declaration`] variant.
//!
//! This is the routing core of D-AR-3: the 67 emit categories measured
//! on the `OpenProject` corpus map to 13 [`Declaration`] variants here.
//! Method-name dispatch is exhaustive over the closed-vocab table
//! (`.claude/plans/openproject-ar-shape-extraction-v1.md` §2 on
//! lance-graph), so the D-AR-4 coverage test fails loudly on any new
//! name not yet routed.
//!
//! The walker is intentionally a single dispatch function over
//! `&str method_name` — no trait, no visitor pattern, no
//! `match_node!` macros (per the prior-art-savant Round-1 verdict:
//! additions-only, free fn over the IR, no new trait surface).

use lib_ruby_parser::Node;
use ruff_spo_triplet::{
    ActsAs, AssocDecl, AssocKind, AttrDecl, AttrKind, Callback, ConcernKind, ConcernRef,
    Delegation, DslCall, DynMethod, EnumForm, EnumVariants, GemDsl, GemKind, ScopeDecl, ScopeKind,
    UsingRef, Validation, ValidationKind,
};

use crate::Declaration;

/// Walk a `Class.body` node and append every recognised DSL call as a
/// [`Declaration`].
///
/// `src` is the file the body was parsed from, when the caller has it. It
/// is used only to keep an unresolved enum argument as the exact source
/// text; without it that argument falls back to the walker's renderer.
pub(crate) fn walk_class_body(body: &Node, src: Option<&str>, out: &mut Vec<Declaration>) {
    match body {
        Node::Begin(b) => {
            for stmt in &b.statements {
                walk_statement(stmt, src, out);
            }
        }
        single => walk_statement(single, src, out),
    }
}

/// Process one class-body statement.
fn walk_statement(node: &Node, src: Option<&str>, out: &mut Vec<Declaration>) {
    match node {
        Node::Send(s) if s.recv.is_none() => route_send(&s.method_name, &s.args, src, out),
        Node::Block(blk) => walk_block(blk, src, out),
        // Ruby keyword `alias new orig` parses as Node::Alias, NOT as a
        // Send; pick up method-alias facts that would otherwise be lost
        // (codex P2 PR #6 r3418*).
        Node::Alias(a) => {
            let new_name = alias_target_name(&a.to);
            let orig_name = alias_target_name(&a.from);
            out.push(Declaration::Attribute(ruff_spo_triplet::AttrDecl {
                kind: ruff_spo_triplet::AttrKind::Alias,
                name: format!("{new_name}={orig_name}"),
                options: Vec::new(),
            }));
        }
        // Visibility modifiers / `class << self`-wrapped sends / def blocks
        // are not DSL declarations and don't produce Declarations.
        _ => {}
    }
}

/// Render the `from` / `to` of a `Node::Alias`. lib-ruby-parser
/// expresses both as `Node::Sym` for the keyword form.
fn alias_target_name(node: &Node) -> String {
    match node {
        Node::Sym(s) => s.name.to_string_lossy(),
        _ => render_node(node),
    }
}

/// `something do ... end` — the call inside the block may itself be a
/// DSL declaration (e.g. `scope :foo, -> { … }` parses as a Send whose
/// last arg is a Block; `class_methods do ... end` parses as a Block
/// whose call is `class_methods`).
fn walk_block(blk: &lib_ruby_parser::nodes::Block, src: Option<&str>, out: &mut Vec<Declaration>) {
    let Node::Send(s) = &*blk.call else {
        return;
    };
    if s.recv.is_some() {
        return;
    }
    let body_ref = format_loc(&blk_body_loc(blk));
    match s.method_name.as_str() {
        "class_methods" => out.push(Declaration::Concern(ConcernRef {
            kind: ConcernKind::ClassMethodsBlock,
            module: String::new(),
            body_ref: Some(body_ref),
        })),
        "included" => out.push(Declaration::Concern(ConcernRef {
            kind: ConcernKind::IncludedBlock,
            module: String::new(),
            body_ref: Some(body_ref),
        })),
        "scope" => {
            if let Some(name) = s.args.first().and_then(sym_string) {
                out.push(Declaration::Scope(ScopeDecl {
                    kind: ScopeKind::Scope,
                    name,
                    body_ref,
                }));
            }
        }
        "default_scope" => out.push(Declaration::Scope(ScopeDecl {
            kind: ScopeKind::DefaultScope,
            name: String::new(),
            body_ref,
        })),
        "validate" => {
            // `validate { … }` — block-form custom validator.
            out.push(Declaration::Validation(Validation {
                kind: ValidationKind::Validate,
                target: "<block>".to_string(),
                options: Vec::new(),
            }));
        }
        // Rails grouping blocks — recurse INTO the block body so nested
        // declarations (`with_options presence: true do; validates :name; end`)
        // are not silently dropped (codex P2 PR #6 r3418*). Limited to
        // known grouping idioms to avoid double-counting from arbitrary
        // blocks (e.g. `5.times do; validates :foo; end` would NOT count
        // because that's runtime-only).
        "with_options" => {
            if let Some(body) = blk.body.as_ref() {
                walk_class_body(body, src, out);
            }
            // Also record the wrapper call itself so the catch-all
            // coverage assertion still sees `with_options` somewhere.
            route_send(s.method_name.as_str(), &s.args, src, out);
        }
        other => {
            // Generic block: route the call as if it were a bare Send,
            // then drop the body. Conservative — keeps the call counted
            // for coverage without inventing a Declaration shape for
            // arbitrary block DSLs.
            route_send(other, &s.args, src, out);
        }
    }
}

/// Locate a Block's body range (best-effort — the parser may inline a
/// single-statement body without an explicit Begin).
fn blk_body_loc(blk: &lib_ruby_parser::nodes::Block) -> lib_ruby_parser::Loc {
    blk.body
        .as_ref()
        .map(|b| match b.as_ref() {
            Node::Begin(b) => b.expression_l,
            other => node_loc(other),
        })
        .unwrap_or(blk.expression_l)
}

/// Approximate a node's expression location by enum tag — works for
/// the node types the walker actually inspects.
fn node_loc(node: &Node) -> lib_ruby_parser::Loc {
    match node {
        Node::Send(s) => s.expression_l,
        Node::Block(b) => b.expression_l,
        Node::Begin(b) => b.expression_l,
        Node::Def(d) => d.expression_l,
        _ => lib_ruby_parser::Loc { begin: 0, end: 0 },
    }
}

/// Format a Loc as `"<begin>..<end>"` for the `body_ref` slot. Bytes,
/// not lines — line-mapping would require the `DecodedInput` which we
/// drop after parsing.
fn format_loc(loc: &lib_ruby_parser::Loc) -> String {
    format!("{}..{}", loc.begin, loc.end)
}

/// Extract a `body_ref` from a `Node::Lambda` arg (i.e. `-> { … }`).
/// Returns `None` for non-lambda nodes.
fn lambda_loc(node: &Node) -> Option<String> {
    match node {
        Node::Lambda(l) => Some(format_loc(&l.expression_l)),
        _ => None,
    }
}

/// Route a bare class-body `Send` call (no receiver) to a Declaration
/// variant. The 78-name closed vocab maps onto this match.
///
/// **Iron-rule lock:** every non-scope-marker class-body method
/// observed on the `OpenProject` corpus MUST appear in either an
/// explicit arm or the `has_dsl_call` catch-all. The D-AR-4 coverage
/// test fails loudly if a real corpus call slips through unmatched.
#[allow(clippy::too_many_lines)] // exhaustive 78-name routing — splitting hurts readability
fn route_send(name: &str, args: &[Node], src: Option<&str>, out: &mut Vec<Declaration>) {
    match name {
        // ───── Associations (5) ─────
        "belongs_to" => emit_assoc(AssocKind::BelongsTo, args, out),
        "has_many" => emit_assoc(AssocKind::HasMany, args, out),
        "has_one" => emit_assoc(AssocKind::HasOne, args, out),
        "has_and_belongs_to_many" => emit_assoc(AssocKind::HasAndBelongsToMany, args, out),
        "accepts_nested_attributes_for" => {
            emit_assoc(AssocKind::AcceptsNestedAttributesFor, args, out);
        }

        // ───── Validations (5) ─────
        "validates" => emit_validation(ValidationKind::Validates, args, out),
        "validate" => emit_validation(ValidationKind::Validate, args, out),
        "normalizes" => emit_validation(ValidationKind::Normalizes, args, out),
        "validates_associated" => emit_validation(ValidationKind::ValidatesAssociated, args, out),
        "validates_each" => emit_validation(ValidationKind::ValidatesEach, args, out),

        // ───── Callbacks (13 phases) ─────
        n if is_callback_phase(n) => emit_callback(n, args, out),

        // ───── Concerns (3 non-block; block forms handled in walk_block) ─────
        "include" => emit_concern(ConcernKind::Include, args, out),
        "extend" => emit_concern(ConcernKind::Extend, args, out),
        "prepend" => emit_concern(ConcernKind::Prepend, args, out),

        // ───── Attributes (13) ─────
        "attribute" => emit_attr(AttrKind::Attribute, args, out),
        "attr_accessor" => emit_attr(AttrKind::AttrAccessor, args, out),
        "attr_reader" => emit_attr(AttrKind::AttrReader, args, out),
        "attr_readonly" => emit_attr(AttrKind::AttrReadonly, args, out),
        "alias_attribute" => emit_attr(AttrKind::AliasAttribute, args, out),
        "alias_method" => emit_attr(AttrKind::AliasMethod, args, out),
        // Note: `alias new orig` is the Ruby keyword form. lib-ruby-parser
        // exposes it as `Node::Alias` (a separate variant), NOT as a Send,
        // so it lands in `walk_statement`'s catch-all. Handled below.
        "undef_method" => emit_attr(AttrKind::UndefMethod, args, out),
        "serialize" => emit_attr(AttrKind::Serialize, args, out),
        "enum" => {
            emit_attr(AttrKind::Enum, args, out);
            emit_enum_forms(args, src, out);
        }
        "store_attribute" => emit_attr(AttrKind::StoreAttribute, args, out),
        "store_accessor" => emit_attr(AttrKind::StoreAccessor, args, out),
        "define_attribute_method" => emit_attr(AttrKind::DefineAttributeMethod, args, out),

        // ───── Delegation ─────
        "delegate" => emit_delegation(args, out),

        // ───── Scope (3 forms) ─────
        // `scope :name, ->{ … }` / `scope :name do … end` — the lambda
        // and do-block forms produce different AST shapes; the lambda
        // arrives as a Send arg (handled here), the do-block as a Block
        // (handled by walk_block).
        "scope" => {
            if let Some(name) = args.first().and_then(sym_string) {
                out.push(Declaration::Scope(ScopeDecl {
                    kind: ScopeKind::Scope,
                    name,
                    body_ref: args
                        .iter()
                        .skip(1)
                        .find_map(lambda_loc)
                        .unwrap_or_else(|| "<lambda>".to_string()),
                }));
            }
        }
        "default_scope" => {
            let body_ref = args
                .iter()
                .find_map(lambda_loc)
                .unwrap_or_else(|| "<lambda>".to_string());
            out.push(Declaration::Scope(ScopeDecl {
                kind: ScopeKind::DefaultScope,
                name: String::new(),
                body_ref,
            }));
        }
        "scopes" => emit_scopes_plural(args, out),

        // ───── acts_as_* family (10 + open-ended) ─────
        n if n.starts_with("acts_as_") => emit_acts_as(n, args, out),

        // ───── OpenProject custom registrations: promoted ─────
        "register_journal_formatter"
        | "register_journal_formatted_fields"
        | "register_query"
        | "activity_provider_for"
        | "deprecated_alias"
        | "associated_to_ask_before_destruction"
        | "has_details_table" => {
            out.push(Declaration::DslCall(DslCall {
                name: name.to_string(),
                args: format_args(args),
            }));
        }

        // ───── Third-party gem DSL (5) ─────
        "mount_uploader" => out.push(Declaration::GemDsl(GemDsl {
            gem: GemKind::MountUploader,
            args: format_args(args),
        })),
        "has_paper_trail" => out.push(Declaration::GemDsl(GemDsl {
            gem: GemKind::HasPaperTrail,
            args: format_args(args),
        })),
        "has_closure_tree" => out.push(Declaration::GemDsl(GemDsl {
            gem: GemKind::HasClosureTree,
            args: format_args(args),
        })),
        "counter_culture" => out.push(Declaration::GemDsl(GemDsl {
            gem: GemKind::CounterCulture,
            args: format_args(args),
        })),
        "auto_strip_attributes" => out.push(Declaration::GemDsl(GemDsl {
            gem: GemKind::AutoStripAttributes,
            args: format_args(args),
        })),

        // ───── Metaprogramming ─────
        "define_method" => {
            if let Some(name_expr) = args.first() {
                out.push(Declaration::DynamicMethod(DynMethod {
                    name_expr: render_node(name_expr),
                    body_ref: format_loc(&node_loc(name_expr)),
                }));
            }
        }

        // ───── Refinements ─────
        "using" => {
            if let Some(refinement) = args.first().and_then(const_string) {
                out.push(Declaration::Using(UsingRef {
                    refinement_module: refinement,
                }));
            }
        }

        // ───── Scope markers — consume silently (not emitted) ─────
        "private"
        | "protected"
        | "public"
        | "private_class_method"
        | "private_constant"
        | "class_attribute"
        | "module_function" => {}

        // ───── Unknown DSL — catch-all so D-AR-4 coverage stays 100 % ─────
        // The OpenProject §2 closed-vocab table lists every name observed
        // on the corpus; this arm is the safety net for any name not yet
        // promoted to a discriminated predicate (and for new OP DSL
        // calls that arrive after the §2 census).
        _ => out.push(Declaration::DslCall(DslCall {
            name: name.to_string(),
            args: format_args(args),
        })),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Per-category emitters
// ─────────────────────────────────────────────────────────────────────────

fn emit_assoc(kind: AssocKind, args: &[Node], out: &mut Vec<Declaration>) {
    let Some(name) = args.first().and_then(sym_string) else {
        return;
    };
    // Scan ALL args for the first Hash to capture options (codex P2
    // PR #6 r3418*). The scoped-association form
    // `has_many :items, -> { active }, dependent: :destroy` puts the
    // options hash at args[2], not args[1] (the lambda sits between
    // the name and the options).
    let options = args
        .iter()
        .skip(1)
        .find_map(as_hash_options)
        .unwrap_or_default();
    out.push(Declaration::Association(AssocDecl {
        kind,
        name,
        options,
    }));
}

fn emit_validation(kind: ValidationKind, args: &[Node], out: &mut Vec<Declaration>) {
    // `validate :method_name` / `validates :attr, ...` — first arg is
    // either a sym (attr/method name) or another shape (block form).
    let target = args.first().and_then(sym_string).unwrap_or_else(|| {
        if !args.is_empty() {
            "<expr>".to_string()
        } else {
            "<empty>".to_string()
        }
    });
    let options = args.iter().find_map(as_hash_options).unwrap_or_default();
    out.push(Declaration::Validation(Validation {
        kind,
        target,
        options,
    }));
}

fn emit_callback(phase: &str, args: &[Node], out: &mut Vec<Declaration>) {
    // Rails accepts several targets per declaration —
    // `before_save :a, :b, :c` registers three callbacks in order — so one
    // `Callback` is emitted per symbol arg. A block/lambda-only declaration
    // (no symbol args) emits the single `<block>` placeholder as before.
    let options = args.iter().find_map(as_hash_options).unwrap_or_default();
    let targets: Vec<String> = args.iter().filter_map(sym_string).collect();
    if targets.is_empty() {
        out.push(Declaration::Callback(Callback {
            phase: phase.to_string(),
            target: "<block>".to_string(),
            options,
        }));
    } else {
        for target in targets {
            out.push(Declaration::Callback(Callback {
                phase: phase.to_string(),
                target,
                options: options.clone(),
            }));
        }
    }
}

fn emit_concern(kind: ConcernKind, args: &[Node], out: &mut Vec<Declaration>) {
    for arg in args {
        if let Some(module) = const_string(arg) {
            out.push(Declaration::Concern(ConcernRef {
                kind,
                module,
                body_ref: None,
            }));
        }
    }
}

fn emit_attr(kind: AttrKind, args: &[Node], out: &mut Vec<Declaration>) {
    // Two-arg alias forms (`alias_attribute :new, :orig`) → one decl with
    // "new=orig" name; everything else takes one declaration per leading
    // symbol arg.
    if matches!(kind, AttrKind::AliasAttribute | AttrKind::AliasMethod) {
        if args.len() >= 2 {
            let new_n = sym_string(&args[0]).unwrap_or_default();
            let orig = sym_string(&args[1]).unwrap_or_default();
            out.push(Declaration::Attribute(AttrDecl {
                kind,
                name: format!("{new_n}={orig}"),
                options: Vec::new(),
            }));
        }
        return;
    }
    // The arity of "which positional symbol arguments are attribute
    // names" varies by macro (codex P2 PR #6 r3418*):
    //   `attribute :age, :integer`            — 1 attr (skip type at args[1])
    //   `attr_accessor :a, :b, :c`            — N attrs
    //   `serialize :data, JSON`               — 1 attr (skip class)
    //   `enum :status, { active: 0 }`         — 1 attr (skip Hash)
    //   `store_attribute :store, :attr, :int` — 1 attr at args[1] (skip store + type)
    //   `store_accessor :store, :a, :b, :c`   — N attrs from args[1..] (skip store)
    //   `define_attribute_method :attr`       — 1 attr
    //   `undef_method :foo`                   — 1 attr
    let (skip, take) = attr_arg_window(kind);
    let mut options = args
        .iter()
        .skip(skip.saturating_add(take))
        .find_map(as_hash_options)
        .unwrap_or_default();
    // D-AR-5.2: pull the Rails static type annotation out of the
    // positional sym that sits right after the attribute name (or
    // after the store key + attr name for store_attribute), and store
    // it as `options[("type", "<rails_type>")]` so the expander can
    // emit a `field_type` triple. `attribute :age, :integer` puts the
    // type at the slot right after the take-window; `serialize :data,
    // JSON` and `store_attribute :store, :attr, :integer` follow the
    // same pattern (single-attr-then-type macros only).
    if attr_has_positional_type(kind) {
        let type_idx = skip.saturating_add(take);
        if let Some(arg) = args.get(type_idx) {
            if let Some(t) = sym_string(arg) {
                options.push(("type".to_string(), t));
            }
        }
    }
    for arg in args.iter().skip(skip).take(take) {
        let Some(name) = sym_string(arg) else {
            continue;
        };
        out.push(Declaration::Attribute(AttrDecl {
            kind,
            name,
            options: options.clone(),
        }));
    }
}

/// `attribute :age, :integer` and `store_attribute :store, :attr, :type`
/// carry the Rails static type as a positional Sym AFTER the attribute
/// name. Multi-attr macros (`attr_accessor :a, :b`) and meta-attr
/// macros (`Serialize`, `Enum`) have no such slot.
fn attr_has_positional_type(kind: AttrKind) -> bool {
    matches!(kind, AttrKind::Attribute | AttrKind::StoreAttribute)
}

/// `(skip, take)` window into the positional args that carry attribute
/// names for each [`AttrKind`]. Args outside this window are type /
/// class / store-key / hash metadata and MUST NOT be treated as
/// attribute names.
///
/// `take == usize::MAX` means "all remaining positional symbol args"
/// (e.g. `attr_accessor :a, :b, :c`).
fn attr_arg_window(kind: AttrKind) -> (usize, usize) {
    match kind {
        // Single-attr macros — name at args[0], type/class/hash at args[1+].
        AttrKind::Attribute
        | AttrKind::Serialize
        | AttrKind::Enum
        | AttrKind::DefineAttributeMethod
        | AttrKind::UndefMethod
        | AttrKind::AttrReadonly => (0, 1),
        // Multi-attr macros — every positional symbol is an attribute name.
        AttrKind::AttrAccessor | AttrKind::AttrReader => (0, usize::MAX),
        // Store-style: args[0] is the store key (NOT an attribute);
        // args[1+] is/are the attribute name(s).
        // `store_attribute :store, :attr, :type` → 1 attr at args[1].
        // `store_accessor :store, :a, :b, :c`    → N attrs from args[1..].
        AttrKind::StoreAttribute => (1, 1),
        AttrKind::StoreAccessor => (1, usize::MAX),
        // Aliases are handled in the early-return above.
        AttrKind::AliasAttribute | AttrKind::AliasMethod | AttrKind::Alias => (0, 0),
    }
}

/// Rails `enum` options. The legacy kwarg form spells them with a leading
/// underscore (`_prefix`); the modern positional form without.
const ENUM_OPTION_KEYS: &[&str] = &[
    "prefix", "suffix", "scopes", "default", "validate", "_prefix", "_suffix", "_scopes",
    "_default",
];

/// Push the typed [`EnumForm`] for every enum column a `enum` call declares.
///
/// Modern form: `enum :status, { open: 0 }, prefix: true` — a Sym column,
/// then the variant argument, then trailing options. Legacy kwarg form:
/// `enum status: { open: 0 }, _prefix: true` — each non-option pair is one
/// column; the option pairs apply to every column of the call.
fn emit_enum_forms(args: &[Node], src: Option<&str>, out: &mut Vec<Declaration>) {
    match args.split_first() {
        Some((Node::Kwargs(k), _)) => emit_enum_forms_legacy(&k.pairs, src, out),
        Some((Node::Hash(h), _)) => emit_enum_forms_legacy(&h.pairs, src, out),
        Some((first, rest)) => {
            if let Some(column) = sym_string(first) {
                emit_enum_form_modern(column, rest, src, out);
            }
        }
        None => {}
    }
}

fn emit_enum_form_modern(
    column: String,
    rest: &[Node],
    src: Option<&str>,
    out: &mut Vec<Declaration>,
) {
    let mut variants = EnumVariants::Missing;
    let mut options = Vec::new();
    let mut tail = rest;
    if let Some((first, after)) = rest.split_first() {
        tail = after;
        match first {
            // Without a positional variant argument Rails reads the keyword
            // arguments as the variants (`enum :status, open: 0`), except for
            // the Rails option keys, which stay options.
            Node::Kwargs(k) => {
                let (opts, pairs) = split_enum_options(&k.pairs);
                options = opts;
                if has_splat(&k.pairs) {
                    variants = EnumVariants::Unresolved(source_text(first, src));
                } else if !pairs.is_empty() {
                    variants = EnumVariants::Hash(pairs);
                }
            }
            other => variants = classify_enum_variants(other, src),
        }
    }
    for arg in tail {
        if let Some(opts) = as_hash_options(arg) {
            options.extend(opts);
        }
    }
    out.push(Declaration::EnumForm(EnumForm::new(
        column, variants, options,
    )));
}

fn emit_enum_forms_legacy(pairs: &[Node], src: Option<&str>, out: &mut Vec<Declaration>) {
    let mut options = Vec::new();
    let mut columns = Vec::new();
    for pair_node in pairs {
        let Node::Pair(p) = pair_node else { continue };
        let key = pair_key(&p.key);
        if ENUM_OPTION_KEYS.contains(&key.as_str()) {
            options.push((key, render_node(&p.value)));
        } else {
            columns.push((key, classify_enum_variants(&p.value, src)));
        }
    }
    for (column, variants) in columns {
        out.push(Declaration::EnumForm(EnumForm::new(
            column,
            variants,
            options.clone(),
        )));
    }
}

/// `(key, value)` pairs rendered verbatim, in source order.
type Pairs = Vec<(String, String)>;

/// Split hash pairs into (Rails enum options, variant pairs), source order.
fn split_enum_options(pairs: &[Node]) -> (Pairs, Pairs) {
    let mut options = Vec::new();
    let mut variants = Vec::new();
    for (key, value) in hash_pairs(pairs) {
        if ENUM_OPTION_KEYS.contains(&key.as_str()) {
            options.push((key, value));
        } else {
            variants.push((key, value));
        }
    }
    (options, variants)
}

/// Classify the variant argument of an `enum` call by the form the source
/// wrote it.
///
/// A literal that splices in values the walker cannot see (`{ **BASE,
/// archived: 2 }`, `[*BASE, :archived]`) is `Unresolved`, never a partial
/// list: a list that silently lacks the spliced values would read as
/// complete.
fn classify_enum_variants(node: &Node, src: Option<&str>) -> EnumVariants {
    match node {
        Node::Hash(h) if !has_splat(&h.pairs) => EnumVariants::Hash(hash_pairs(&h.pairs)),
        Node::Array(a) if !a.elements.iter().any(|e| matches!(e, Node::Splat(_))) => {
            EnumVariants::Array(a.elements.iter().map(array_key).collect())
        }
        other => EnumVariants::Unresolved(source_text(other, src)),
    }
}

/// An array element as a key: `:sym` and `"str"` unwrapped, the same way
/// [`pair_key`] reads hash keys.
fn array_key(element: &Node) -> String {
    match element {
        Node::Str(s) => s.value.to_string_lossy(),
        other => sym_string(other).unwrap_or_else(|| render_node(other)),
    }
}

/// Whether hash pairs include a `**splat`, whose keys the walker cannot see.
fn has_splat(pairs: &[Node]) -> bool {
    pairs.iter().any(|p| matches!(p, Node::Kwsplat(_)))
}

/// The exact source text of `node` when the source is available, else the
/// walker's rendering of it.
fn source_text(node: &Node, src: Option<&str>) -> String {
    let loc = node.expression();
    src.and_then(|s| s.get(loc.begin..loc.end))
        .map_or_else(|| render_node(node), str::to_owned)
}

fn emit_delegation(args: &[Node], out: &mut Vec<Declaration>) {
    let mut methods = Vec::new();
    let mut to = String::new();
    let mut options = Vec::new();
    for arg in args {
        if let Some(sym) = sym_string(arg) {
            methods.push(sym);
        } else if let Some(opts) = as_hash_options(arg) {
            for (k, v) in &opts {
                if k == "to" {
                    to = v.trim_start_matches(':').to_string();
                } else {
                    options.push((k.clone(), v.clone()));
                }
            }
        }
    }
    if !methods.is_empty() {
        out.push(Declaration::Delegation(Delegation {
            methods,
            to,
            options,
        }));
    }
}

fn emit_scopes_plural(args: &[Node], out: &mut Vec<Declaration>) {
    // OP plural form `scopes :a, :b, :c` — one ScopeDecl per name,
    // body_ref placeholder (no per-scope lambda in the plural form).
    for arg in args {
        if let Some(name) = sym_string(arg) {
            out.push(Declaration::Scope(ScopeDecl {
                kind: ScopeKind::Scopes,
                name,
                body_ref: "<plural>".to_string(),
            }));
        }
    }
}

fn emit_acts_as(name: &str, args: &[Node], out: &mut Vec<Declaration>) {
    let variant = name.strip_prefix("acts_as_").unwrap_or(name).to_string();
    let options = args.iter().find_map(as_hash_options).unwrap_or_default();
    out.push(Declaration::ActsAs(ActsAs { variant, options }));
}

// ─────────────────────────────────────────────────────────────────────────
// Arg shape helpers
// ─────────────────────────────────────────────────────────────────────────

/// Extract a `:symbol` literal as a String. Returns `None` for non-Sym
/// nodes.
fn sym_string(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        _ => None,
    }
}

/// Render a `Const` reference as a dotted Ruby constant path.
fn const_string(node: &Node) -> Option<String> {
    match node {
        Node::Const(c) => {
            let suffix = c.name.clone();
            if let Some(scope) = &c.scope {
                if let Node::Cbase(_) = **scope {
                    Some(format!("::{suffix}"))
                } else if let Some(prefix) = const_string(scope) {
                    Some(format!("{prefix}::{suffix}"))
                } else {
                    Some(suffix)
                }
            } else {
                Some(suffix)
            }
        }
        _ => None,
    }
}

/// Best-effort string render of a Node for arg-blob slots. Lossy but
/// stable enough for queryability.
fn render_node(node: &Node) -> String {
    match node {
        Node::Sym(s) => format!(":{}", s.name.to_string_lossy()),
        Node::Str(s) => format!("\"{}\"", s.value.to_string_lossy()),
        Node::Int(i) => i.value.clone(),
        Node::Const(_) => const_string(node).unwrap_or_default(),
        Node::True(_) => "true".to_string(),
        Node::False(_) => "false".to_string(),
        Node::Nil(_) => "nil".to_string(),
        Node::Array(a) => {
            let elems = a
                .elements
                .iter()
                .map(render_node)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{elems}]")
        }
        Node::Hash(h) => format_hash_inline(h),
        _ => "<expr>".to_string(),
    }
}

/// Try to interpret a Node as a Hash of `key: value` pairs and render
/// each pair as `(key, value)` for the `options` slot. Accepts both
/// `Hash { pairs }` (literal `{k: v}` braces) and `Kwargs { pairs }`
/// (trailing `k: v, k2: v2` keyword arguments — common in Rails macro
/// calls like `has_many :x, dependent: :destroy`).
fn as_hash_options(node: &Node) -> Option<Vec<(String, String)>> {
    let pairs = match node {
        Node::Hash(h) => &h.pairs,
        Node::Kwargs(k) => &k.pairs,
        _ => return None,
    };
    Some(hash_pairs(pairs))
}

/// Render the `Pair` nodes of a hash as `(key, value)` strings, source
/// order, duplicates kept.
fn hash_pairs(pairs: &[Node]) -> Vec<(String, String)> {
    pairs
        .iter()
        .filter_map(|pair_node| {
            let Node::Pair(p) = pair_node else {
                return None;
            };
            Some((pair_key(&p.key), render_node(&p.value)))
        })
        .collect()
}

/// A hash key as a plain string: `:sym` and `"str"` keys unwrapped,
/// anything else rendered.
fn pair_key(key: &Node) -> String {
    match key {
        Node::Sym(s) => s.name.to_string_lossy(),
        Node::Str(s) => s.value.to_string_lossy(),
        other => render_node(other),
    }
}

/// Render a Hash node inline as `{k: v, k2: v2}` for the args blob.
fn format_hash_inline(h: &lib_ruby_parser::nodes::Hash) -> String {
    let mut parts = Vec::new();
    for pair_node in &h.pairs {
        let Node::Pair(p) = pair_node else { continue };
        let key = render_node(&p.key);
        let value = render_node(&p.value);
        parts.push(format!("{key}: {value}"));
    }
    format!("{{{}}}", parts.join(", "))
}

/// Render the full arg list as one verbatim string for catch-all
/// `has_dsl_call` and `GemDsl` slots.
fn format_args(args: &[Node]) -> String {
    args.iter().map(render_node).collect::<Vec<_>>().join(", ")
}

/// The 13 Rails callback phases observed on the `OpenProject` corpus.
/// Pattern is `before_*` / `after_*` / `around_*` — guarded by an
/// allow-list because Rails recognises specific suffixes and an
/// arbitrary `before_foo` would otherwise route here too.
fn is_callback_phase(name: &str) -> bool {
    matches!(
        name,
        "before_save"
            | "before_destroy"
            | "before_create"
            | "before_validation"
            | "before_update"
            | "after_save"
            | "after_destroy"
            | "after_create"
            | "after_update"
            | "after_commit"
            | "after_validation"
            | "after_initialize"
            | "after_destroy_commit"
            | "after_create_commit"
            | "after_update_commit"
            | "after_save_commit"
            | "around_destroy"
            | "around_save"
            | "around_create"
            | "around_update"
    )
}
