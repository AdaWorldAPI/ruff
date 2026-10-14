//! The Rails `config/routes.rb` harvest arm — the routing stratum (gap
//! (b) from the RAILS-COVERAGE-KIT gap ledger). Mints the missing hop:
//! **helper stem → `controller#action`**, so button → route → controller
//! → mutation traces (`InvokesAction`'s object today resolves to nothing)
//! become joinable for the first time. See `.claude/knowledge/
//! fuzzy-recipe-codebook.md` §8c ("config.json becomes DATA") — a Rails
//! `routes.rb` DSL call is config-as-data, so this is data-ingestion, not
//! codegen.
//!
//! # AST walk, not a line scanner
//!
//! Unlike [`crate::navigation`] / [`crate::actions`] (closed-vocab ERB
//! line scanners), route declarations nest arbitrarily (`namespace` /
//! `scope` / `resources` / `member` / `collection` / `if`/`unless`
//! guards), so this arm walks the `lib-ruby-parser` AST directly: a scope
//! stack of [`Frame`]s threads `namespace`/`scope`/`resources`/`resource`/
//! `controller`/`member`/`collection` context through the recursive
//! descent, and every `if`/`unless`/`do…end`/`{…}` wrapper is transparent
//! (the walker descends into both branches of a conditional and treats
//! block-call bodies uniformly regardless of brace-vs-do syntax).
//!
//! # What is captured
//!
//! - `resources`/`resource` canonical actions (the filtered-by-`only`/
//!   `except` seven/six, respecting `shallow:`, `module:`, `controller:`,
//!   `as:`, multi-name replay).
//! - `member`/`collection` block verb calls, `on:` kwarg verb calls, and
//!   naked verb calls nested directly in a resource body (Rails' implicit
//!   member/nested-resource conventions).
//! - Standalone verb calls (`get`/`post`/`put`/`patch`/`delete`/`match`),
//!   resolving `to:` / fat-arrow / `action:` + ambient-controller forms.
//! - `concern` define + `concerns` replay (call form and array/kwarg
//!   form), same-file only.
//! - `root`, `match … via: [...]` expansion.
//!
//! # What is NOT captured (see the frozen spec's §0 non-goals)
//!
//! Return shape (`respond_to`), redirects (both forms), mounts (both
//! forms), proc/lambda endpoints, dynamically-generated routes (`.each`
//! loops with interpolated names), constraint semantics, `direct`/
//! `resolve`/`draw`, full Rails inflection (see [`IRREGULAR`] /
//! [`pluralize`]), `APIv3` (Grape). Every one of these is counted in
//! [`RouteScanReport`], never silently dropped.
//!
//! # Correctness rule
//!
//! A stem this arm emits MUST be one Rails actually generates, else it
//! emits no triple and counts the miss — never a guess.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use lib_ruby_parser::{Node, Parser, ParserOptions};
use ruff_spo_triplet::{Predicate, Provenance, Triple};

use crate::actions::ActionVerb;

/// The mutating HTTP verb an action affordance carries. GET is deliberately
/// absent — a GET affordance is navigation (`crate::navigation`), not an
/// action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RouteVerb {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl RouteVerb {
    /// Parse a Rails HTTP-verb token (`"get"`, `:patch`, …) into a
    /// [`RouteVerb`]. Case- and sigil-tolerant (`:get`, `"get"`, `get`).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw
            .trim()
            .trim_start_matches(':')
            .trim_matches('"')
            .trim_matches('\'')
        {
            "get" => Some(Self::Get),
            "post" => Some(Self::Post),
            "put" => Some(Self::Put),
            "patch" => Some(Self::Patch),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// The lowercase HTTP verb — pinned lowercase (matches
    /// `ActionVerb::as_str` + the `HasCallback` lowercase discipline).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
        }
    }
}

/// `ActionVerb` has no `Get` by design (it is the mutating-only subset);
/// every other verb maps straight across.
impl From<ActionVerb> for RouteVerb {
    fn from(v: ActionVerb) -> Self {
        match v {
            ActionVerb::Post => Self::Post,
            ActionVerb::Put => Self::Put,
            ActionVerb::Patch => Self::Patch,
            ActionVerb::Delete => Self::Delete,
        }
    }
}

/// The Rails route-scope style a [`RouteEntry`] was declared under —
/// which family of helper-stem derivation rule applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScopeKind {
    /// A `member do…end` block, an `on: :member` kwarg, or (singleton
    /// `resource`) a naked verb call in the resource body.
    Member,
    /// A `collection do…end` block or an `on: :collection` kwarg.
    Collection,
    /// One of the `resources`/`resource` filtered canonical actions
    /// (index/create/new/edit/show/update/destroy).
    Canonical,
    /// Not under any `resources`/`resource` — a bare top-level (or
    /// `namespace`/`scope`-wrapped) verb call.
    Standalone,
}

impl RouteScopeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Collection => "collection",
            Self::Canonical => "canonical",
            Self::Standalone => "standalone",
        }
    }
}

/// One harvested route declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    /// The full Rails helper stem this route generates (the literal token
    /// Rails' route-helper generator produces — the same join contract
    /// `crate::actions::action_target` uses, no `new_`/`edit_`
    /// stripping). `None` when no helper is derivable (a bare
    /// dynamic-segment string path with no `as:`).
    pub stem: Option<String>,
    pub verb: RouteVerb,
    /// Module-qualified controller path (`"work_packages"`,
    /// `"admin/users"`).
    pub controller: String,
    pub action: String,
    pub scope: RouteScopeKind,
    /// Path relative to the corpus root, `/`-joined.
    pub file: String,
}

/// The full set of routes harvested from one corpus. `resolve` is THE
/// joint: helper stem (+ verb, since canonical show/update/destroy share
/// a stem) → controller#action.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    pub entries: Vec<RouteEntry>,
}

impl RouteTable {
    /// Resolve a helper stem + verb to its controller#action. Pure lookup
    /// — composition stays in consumers (`navigation.rs:9-13` discipline).
    /// Returns the FIRST matching entry (declaration order) — the C3
    /// tie-break for a later same-(stem,verb) conflict.
    #[must_use]
    pub fn resolve(&self, stem: &str, verb: RouteVerb) -> Option<&RouteEntry> {
        self.entries
            .iter()
            .find(|e| e.stem.as_deref() == Some(stem) && e.verb == verb)
    }
}

/// Conservation-ledger totals for a routes scan — the honest denominator
/// discipline shared with [`crate::actions::ActionScanReport`] /
/// [`crate::navigation::NavScanReport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteScanReport {
    /// `config/routes.rb` + every `modules/*/config/routes.rb` found.
    pub files_scanned: usize,
    /// Every route-declaration unit processed (one per verb — a
    /// `via: [:get, :post]` expands to two; `via: :all` is one opaque
    /// site) — the total the identity in [`Self`]'s doc balances against.
    pub declared_routes: usize,
    /// [`RouteEntry`] records with `stem.is_some()` — the triple-emitting
    /// subset.
    pub entries_emitted: usize,
    /// [`RouteEntry`] records with `stem.is_none()` — recorded (so
    /// consumers still see controller/action/verb), no triple.
    pub entries_without_stem: usize,
    pub escaped_redirects: usize,
    pub escaped_mounts: usize,
    pub escaped_procs: usize,
    pub escaped_via_all: usize,
    /// Non-literal name/path/`as:`/`controller:`/`action:`/`to:` values —
    /// a route whose target can't be statically resolved (`.each` loops,
    /// non-literal kwarg values).
    pub escaped_dynamic: usize,
    /// Unknown DSL call names, deduped (never `direct`/`resolve`/`draw` —
    /// zero corpus occurrences, per the frozen spec's non-goals).
    pub escaped_other: Vec<String>,
    /// A later declaration reusing an already-seen (stem, verb) with a
    /// DIFFERENT controller#action — the entry stays in `entries`, but no
    /// second `RoutesTo` triple is emitted (`resolve` keeps returning the
    /// first).
    pub duplicate_stem_conflicts: usize,
}

/// Scan `<root>/config/routes.rb` + every `<root>/modules/*/config/
/// routes.rb`. Thin wrapper over [`extract_routes_with_report`].
#[must_use]
pub fn extract_routes(root: &Path, namespace: &str) -> RouteTable {
    extract_routes_with_report(root, namespace).0
}

/// Like [`extract_routes`] but also returns the [`RouteScanReport`].
///
/// `namespace` is accepted for signature parity with the crate's other
/// `extract_*(tree, namespace)` entry points (`extract_with`,
/// `extract_app_with`) and is the namespace a caller subsequently passes
/// to [`RouteTable::to_triples`] — [`RouteEntry`]/[`RouteTable`] stay
/// namespace-free (bare stems), so it is not consumed inside this scan.
#[must_use]
pub fn extract_routes_with_report(root: &Path, _namespace: &str) -> (RouteTable, RouteScanReport) {
    let mut table = RouteTable::default();
    let mut report = RouteScanReport::default();
    let mut seen_stem_verb: Vec<(String, RouteVerb, String)> = Vec::new();

    for path in collect_route_files(root) {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = relative_path(root, &path);
        report.files_scanned += 1;
        walk_route_file(&src, &rel, &mut table, &mut report, &mut seen_stem_verb);
    }
    report.entries_emitted = table.entries.iter().filter(|e| e.stem.is_some()).count();
    report.entries_without_stem = table.entries.iter().filter(|e| e.stem.is_none()).count();
    report.escaped_other.sort();
    report.escaped_other.dedup();
    (table, report)
}

/// `<root>/config/routes.rb` + every `<root>/modules/*/config/routes.rb`,
/// filename-sorted for determinism.
fn collect_route_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let core = root.join("config/routes.rb");
    if core.is_file() {
        files.push(core);
    }
    if let Ok(entries) = fs::read_dir(root.join("modules")) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("config/routes.rb");
            if candidate.is_file() {
                files.push(candidate);
            }
        }
    }
    files.sort();
    files
}

fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ─────────────────────────────────────────────────────────────────────────
// The scope stack
// ─────────────────────────────────────────────────────────────────────────

/// One frame of the route-DSL scope stack (v3 §A). `Resources`/`Resource`
/// carry PRE-COMPUTED tokens (not raw plural/singular) per v3's structural
/// fix: `member_token` for member/new/edit-style stems, `collection_token`
/// for index/create/collection-style stems, `controller` fully resolved
/// (prefix + `controller:`/module: override already applied) at push time.
#[derive(Debug, Clone)]
enum Frame {
    Resources {
        collection_token: String,
        member_token: String,
        shallow: bool,
        controller: String,
    },
    Resource {
        collection_token: String,
        member_token: String,
        controller: String,
    },
    Namespace {
        name: String,
    },
    Scope {
        as_prefix: Option<String>,
        module: Option<String>,
        controller: Option<String>,
    },
    Controller {
        name: String,
    },
    Member,
    Collection,
}

/// `helper_prefix` — outer→inner join (`_`) of every `Namespace.name` and
/// every `Scope.as_prefix` (Some only), trailing `_` included (empty when
/// no contributor).
fn helper_prefix(stack: &[Frame]) -> String {
    let mut parts = Vec::new();
    for f in stack {
        match f {
            Frame::Namespace { name } => parts.push(name.clone()),
            Frame::Scope {
                as_prefix: Some(p), ..
            } => parts.push(p.clone()),
            _ => {}
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}_", parts.join("_"))
    }
}

/// `controller_prefix` — outer→inner join (`/`) of every `Namespace.name`
/// and `Scope.module` (Some only), trailing `/` included.
fn controller_prefix(stack: &[Frame]) -> String {
    let mut parts = Vec::new();
    for f in stack {
        match f {
            Frame::Namespace { name } => parts.push(name.clone()),
            Frame::Scope {
                module: Some(m), ..
            } => parts.push(m.clone()),
            _ => {}
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}/", parts.join("/"))
    }
}

/// `ambient_controller` — innermost `Controller.name` frame, or innermost
/// `Scope.controller` (Some), searching from the top of the stack inward.
/// Used ONLY for standalone (no enclosing `Resources`/`Resource`) verb
/// calls.
fn ambient_controller(stack: &[Frame]) -> Option<String> {
    for f in stack.iter().rev() {
        match f {
            Frame::Controller { name } => return Some(name.clone()),
            Frame::Scope {
                controller: Some(c),
                ..
            } => return Some(c.clone()),
            _ => {}
        }
    }
    None
}

/// The index (within `stack`) of the innermost `Resources`/`Resource`
/// frame, if any — "the current resource".
fn innermost_resource_idx(stack: &[Frame]) -> Option<usize> {
    stack
        .iter()
        .rposition(|f| matches!(f, Frame::Resources { .. } | Frame::Resource { .. }))
}

/// `parent_resource_prefix` — join (`_`) of every `Resources`/`Resource`
/// frame STRICTLY BEFORE `upto` (the ancestors of "the current resource"),
/// using each ancestor's `member_token` (parent path segments are always
/// singular). For `member_style` stems, an ancestor with `shallow: true`
/// drops its OWN contribution (v3 §3 shallow rule); collection/new-style
/// stems always keep every ancestor.
fn parent_resource_prefix(stack: &[Frame], upto: usize, member_style: bool) -> String {
    let mut parts = Vec::new();
    for f in &stack[..upto] {
        match f {
            Frame::Resources {
                member_token,
                shallow,
                ..
            } => {
                if member_style && *shallow {
                    continue;
                }
                parts.push(member_token.clone());
            }
            Frame::Resource { member_token, .. } => parts.push(member_token.clone()),
            _ => {}
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}_", parts.join("_"))
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The walker
// ─────────────────────────────────────────────────────────────────────────

enum EscapeKind {
    Redirect,
    Mount,
    Proc,
    ViaAll,
    Dynamic,
    Other(String),
}

struct Walker<'a> {
    file: String,
    stack: Vec<Frame>,
    /// `concern :name do … end` definitions seen so far (same-file only,
    /// define-before-use — every corpus site follows this order).
    concerns: HashMap<String, &'a Node>,
    entries: &'a mut Vec<RouteEntry>,
    report: &'a mut RouteScanReport,
    /// Running (stem, verb) → "controller#action" ledger for the C3
    /// duplicate-conflict tie-break.
    seen: &'a mut Vec<(String, RouteVerb, String)>,
}

impl Walker<'_> {
    /// Record one resolved route declaration. Increments
    /// `declared_routes` unconditionally — every call site that reaches
    /// either `emit` or `escape` accounts for exactly one declared route
    /// (the F3 identity is an invariant of this being the only two entry
    /// points, never a separately-tracked counter).
    fn emit(
        &mut self,
        stem: Option<String>,
        verb: RouteVerb,
        controller: String,
        action: String,
        scope: RouteScopeKind,
    ) {
        self.report.declared_routes += 1;
        if let Some(s) = &stem {
            let key = format!("{controller}#{action}");
            if let Some(existing) = self.seen.iter().find(|(st, v, _)| st == s && *v == verb) {
                if existing.2 != key {
                    self.report.duplicate_stem_conflicts += 1;
                }
            } else {
                self.seen.push((s.clone(), verb, key));
            }
        }
        self.entries.push(RouteEntry {
            stem,
            verb,
            controller,
            action,
            scope,
            file: self.file.clone(),
        });
    }

    fn escape(&mut self, which: EscapeKind) {
        self.report.declared_routes += 1;
        match which {
            EscapeKind::Redirect => self.report.escaped_redirects += 1,
            EscapeKind::Mount => self.report.escaped_mounts += 1,
            EscapeKind::Proc => self.report.escaped_procs += 1,
            EscapeKind::ViaAll => self.report.escaped_via_all += 1,
            EscapeKind::Dynamic => self.report.escaped_dynamic += 1,
            EscapeKind::Other(name) => self.report.escaped_other.push(name),
        }
    }
}

fn walk_route_file(
    src: &str,
    file: &str,
    table: &mut RouteTable,
    report: &mut RouteScanReport,
    seen: &mut Vec<(String, RouteVerb, String)>,
) {
    let options = ParserOptions {
        buffer_name: file.to_string(),
        ..Default::default()
    };
    let parser = Parser::new(src.as_bytes().to_vec(), options);
    let Some(ast) = parser.do_parse().ast.map(|b| *b) else {
        return;
    };
    let Some(body) = find_draw_body(&ast) else {
        return;
    };
    let mut w = Walker {
        file: file.to_string(),
        stack: Vec::new(),
        concerns: HashMap::new(),
        entries: &mut table.entries,
        report,
        seen,
    };
    walk_stmt(body, &mut w);
}

/// Find `Rails.application.routes.draw do … end`'s body. Matched on the
/// `draw` method name alone (not the full receiver chain) — every corpus
/// file uses exactly this call once, at the top level.
fn find_draw_body(node: &Node) -> Option<&Node> {
    match node {
        Node::Block(b) => {
            if let Node::Send(s) = b.call.as_ref() {
                if s.method_name == "draw" {
                    return b.body.as_deref();
                }
            }
            None
        }
        Node::Begin(b) => b.statements.iter().find_map(find_draw_body),
        _ => None,
    }
}

/// Walk one class-body-shaped statement (Begin-flattened, `if`/`unless`
/// transparent). This is the whole-file top-level walker AND the body
/// walker for every block (`resources do…end`, `member do…end`, …) — the
/// scope stack is what differentiates behaviour, not a separate function
/// per nesting kind.
fn walk_stmt<'a>(node: &'a Node, w: &mut Walker<'a>) {
    match node {
        Node::Begin(b) => {
            for stmt in &b.statements {
                walk_stmt(stmt, w);
            }
        }
        Node::If(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_stmt(t, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_stmt(f, w);
            }
        }
        Node::IfMod(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_stmt(t, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_stmt(f, w);
            }
        }
        Node::Block(blk) => {
            if let Node::Send(s) = blk.call.as_ref() {
                if s.recv.is_none() {
                    dispatch(&s.method_name, &s.args, blk.body.as_deref(), w);
                }
            }
        }
        Node::Send(s) if s.recv.is_none() => {
            dispatch(&s.method_name, &s.args, None, w);
        }
        _ => {}
    }
}

/// Route one bare-or-block class-body call by method name. `block_body`
/// is `Some` for `name do … end` / `name { … }`, `None` for a plain
/// (argument-only) call.
#[allow(clippy::too_many_lines)]
fn dispatch<'a>(name: &str, args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    match name {
        "resources" => handle_resources(args, block_body, w),
        "resource" => handle_resource(args, block_body, w),
        "namespace" => handle_namespace(args, block_body, w),
        "scope" => handle_scope(args, block_body, w),
        "controller" => handle_controller(args, block_body, w),
        "member" => {
            w.stack.push(Frame::Member);
            if let Some(body) = block_body {
                walk_stmt(body, w);
            }
            w.stack.pop();
        }
        "collection" => {
            w.stack.push(Frame::Collection);
            if let Some(body) = block_body {
                walk_stmt(body, w);
            }
            w.stack.pop();
        }
        "concern" => handle_concern_def(args, block_body, w),
        "concerns" => handle_concerns_replay(args, w),
        "get" | "post" | "put" | "patch" | "delete" => {
            let Some(verb) = RouteVerb::parse(name) else {
                return;
            };
            handle_verb(verb, args, w);
        }
        "match" => handle_match(args, w),
        "root" => handle_root(args, w),
        "mount" => w.escape(EscapeKind::Mount),
        "redirect" => w.escape(EscapeKind::Redirect),
        "constraints" => {
            // Constraints are transparent — routes inside parsed normally
            // (v3 §0: "constraint semantics" out of scope, bodies aren't).
            if let Some(body) = block_body {
                walk_stmt(body, w);
            }
        }
        "with_options" | "shallow" => {
            // Grouping wrappers — descend, no scope-stack effect of their
            // own (a real `shallow do…end` block would need per-resource
            // ambient-shallow inheritance; unexercised on this corpus).
            if let Some(body) = block_body {
                walk_stmt(body, w);
            }
        }
        "direct" | "resolve" | "draw" => {
            w.escape(EscapeKind::Other(name.to_string()));
        }
        // Rack-app-mount idioms and anything else unrecognised.
        _ => {
            if block_body.is_some() || !args.is_empty() {
                w.escape(EscapeKind::Other(name.to_string()));
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Arg-shape helpers
// ─────────────────────────────────────────────────────────────────────────

/// A `:symbol` literal as a String.
fn sym_str(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        _ => None,
    }
}

/// A `"string"` literal as a String (plain `Str`, not an interpolated
/// `Dstr` — those are non-literal by construction).
fn str_lit(node: &Node) -> Option<String> {
    match node {
        Node::Str(s) => Some(s.value.to_string_lossy()),
        _ => None,
    }
}

/// Either sigil: a Sym or a Str, unwrapped to its bare text.
fn sym_or_str(node: &Node) -> Option<String> {
    sym_str(node).or_else(|| str_lit(node))
}

/// `true`/`false` literal.
fn bool_lit(node: &Node) -> Option<bool> {
    match node {
        Node::True(_) => Some(true),
        Node::False(_) => Some(false),
        _ => None,
    }
}

/// A bare Rails-identifier token: `[a-z_][a-z0-9_]*`. Distinguishes a
/// stem-derivable name (`"resend_invite"`) from a slash/dynamic path
/// (`"/roadmap"`, `"/:tab"`) at the "single-segment identifier" level —
/// [`path_auto_name`] (C1) handles the slash/dash-bearing superset.
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// C1 — Rails auto-names a `/`/word string path: `^/?[-a-z0-9_/]+$` (no
/// `:`, `(`, `*`, `.`). Strips the leading `/`, maps `/` and `-` → `_`.
/// `None` for anything containing a dynamic (`:x`), glob (`*x`), format
/// (`.x`), or optional-segment (`(`) marker.
fn path_auto_name(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    if raw
        .chars()
        .any(|c| matches!(c, ':' | '(' | ')' | '*' | '.'))
    {
        return None;
    }
    let body = raw.strip_prefix('/').unwrap_or(raw);
    if body.is_empty() {
        // A bare `/` contributes nothing (B4/C1: the collection candidate
        // falls back to the resource's own token).
        return Some(String::new());
    }
    if !body
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/' || c == '-')
    {
        return None;
    }
    Some(body.replace(['/', '-'], "_"))
}

/// Every `(key, value_node)` pair across all `Hash`/`Kwargs` args, in
/// declaration order. Both the braced-literal (`Hash`) and trailing
/// keyword-sugar (`Kwargs`) shapes are accepted (Rails route calls use
/// both freely).
fn kwarg_pairs(args: &[Node]) -> Vec<(String, &Node)> {
    let mut out = Vec::new();
    for arg in args {
        let pairs = match arg {
            Node::Hash(h) => &h.pairs,
            Node::Kwargs(k) => &k.pairs,
            _ => continue,
        };
        for pair_node in pairs {
            let Node::Pair(p) = pair_node else { continue };
            let key = match p.key.as_ref() {
                Node::Sym(s) => s.name.to_string_lossy(),
                Node::Str(s) => s.value.to_string_lossy(),
                _ => continue,
            };
            out.push((key, p.value.as_ref()));
        }
    }
    out
}

fn kwarg<'n>(pairs: &[(String, &'n Node)], key: &str) -> Option<&'n Node> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}

/// Route kwarg vocabulary — used to distinguish a genuine `key: value`
/// keyword arg from the fat-arrow "path => target" pseudo-pair (whose key
/// is a free-form path/action string, never one of these reserved names).
const ROUTE_KWARGS: &[&str] = &[
    "to",
    "as",
    "via",
    "on",
    "action",
    "controller",
    "constraints",
    "defaults",
    "format",
    "path",
    "param",
    "path_names",
    "concerns",
    "only",
    "except",
    "shallow",
    "module",
    "anchor",
    "direct",
    "internal",
    "if",
    "unless",
];

/// The `"path" => "ctrl#action"` fat-arrow pseudo-pair: the first
/// Hash/Kwargs arg's first pair whose key is NOT a [`ROUTE_KWARGS`] name.
fn fat_arrow_pair(args: &[Node]) -> Option<(&Node, &Node)> {
    for arg in args {
        let pairs = match arg {
            Node::Hash(h) => &h.pairs,
            Node::Kwargs(k) => &k.pairs,
            _ => continue,
        };
        for pair_node in pairs {
            let Node::Pair(p) = pair_node else { continue };
            let is_reserved = match p.key.as_ref() {
                Node::Sym(s) => ROUTE_KWARGS.contains(&s.name.to_string_lossy().as_str()),
                Node::Str(s) => ROUTE_KWARGS.contains(&s.value.to_string_lossy().as_str()),
                _ => false,
            };
            if !is_reserved {
                return Some((p.key.as_ref(), p.value.as_ref()));
            }
        }
    }
    None
}

/// True positional args — everything in `args` that is NOT a `Hash`/
/// `Kwargs` node.
fn positional_args(args: &[Node]) -> Vec<&Node> {
    args.iter()
        .filter(|a| !matches!(a, Node::Hash(_) | Node::Kwargs(_)))
        .collect()
}

/// B1 — an absolute `controller:` (or `to:`) value: a leading `/` strips
/// the slash and is used VERBATIM, discarding `controller_prefix`;
/// otherwise the value is prefixed with `controller_prefix`.
fn apply_controller_override(raw: &str, controller_prefix: &str) -> String {
    if let Some(stripped) = raw.strip_prefix('/') {
        stripped.to_string()
    } else {
        format!("{controller_prefix}{raw}")
    }
}

/// Split a literal `"controller#action"` string. `None` if there is no
/// `#`.
fn split_ctrl_action(raw: &str) -> Option<(&str, &str)> {
    raw.split_once('#')
}

/// Is `node` a `redirect(...)` / `redirect { … }` call (either arg-form
/// or block-form)?
fn is_redirect_call(node: &Node) -> bool {
    is_bare_call(node, "redirect")
}

/// Is `node` a `proc { … }` / `lambda { … }` call?
fn is_proc_call(node: &Node) -> bool {
    is_bare_call(node, "proc") || is_bare_call(node, "lambda")
}

fn is_bare_call(node: &Node, name: &str) -> bool {
    match node {
        Node::Send(s) => s.recv.is_none() && s.method_name == name,
        Node::Block(b) => {
            matches!(b.call.as_ref(), Node::Send(s) if s.recv.is_none() && s.method_name == name)
        }
        _ => false,
    }
}

/// An `only:`/`except:` filter value as a set of action-name strings.
/// Accepts a bare Symbol/String, or an Array of either — `%i[]`/`%w[]`
/// literals are normalised to `Array` by lib-ruby-parser at parse time,
/// so no separate handling is needed for D1.
fn action_set(node: &Node) -> Vec<String> {
    match node {
        Node::Array(a) => a.elements.iter().filter_map(sym_or_str).collect(),
        other => sym_or_str(other).into_iter().collect(),
    }
}

/// The filtered subset of `actions` per an `only:`/`except:` kwarg pair
/// (at most one is meaningful; `only:` takes priority if — non-standard —
/// both are somehow present).
fn allowed_actions(only: Option<&Node>, except: Option<&Node>, actions: &[&str]) -> Vec<String> {
    if let Some(o) = only {
        let allow = action_set(o);
        return actions
            .iter()
            .map(|s| (*s).to_string())
            .filter(|a| allow.contains(a))
            .collect();
    }
    if let Some(e) = except {
        let deny = action_set(e);
        return actions
            .iter()
            .map(|s| (*s).to_string())
            .filter(|a| !deny.contains(a))
            .collect();
    }
    actions.iter().map(|s| (*s).to_string()).collect()
}

const CANONICAL_ACTIONS: &[&str] = &[
    "index", "create", "new", "edit", "show", "update", "destroy",
];
/// Singleton `resource` canonical actions — the seven minus `index` (Rails
/// `SingletonResource` has no index route).
const SINGLETON_ACTIONS: &[&str] = &["create", "new", "edit", "show", "update", "destroy"];

/// Resource names whose singular/plural isn't rule-derivable. Duplicated
/// from `schema.rs`'s fn-local `IRREGULAR` (E4: `schema::singularize`'s
/// ALGORITHM is shared via `pub(crate)`; its private table is not, so the
/// pairs are copied here).
const IRREGULAR: &[(&str, &str)] = &[
    ("news", "news"),
    ("meeting_agenda_item_series", "meeting_agenda_item_series"),
];

fn singularize_local(word: &str) -> String {
    crate::schema::singularize(word, word, IRREGULAR)
}

/// Routes.rs-LOCAL pluralisation (E4) — schema.rs has no `pluralize`.
/// Rails' basic count-noun rule: consonant+`y` → `ies`; the irregular
/// table (reversed) short-circuits; `s`/`x`/`z`/`ch`/`sh` tails → `es`;
/// else a plain `s` suffix.
fn pluralize_local(word: &str) -> String {
    if let Some((_, plural)) = IRREGULAR.iter().find(|(singular, _)| *singular == word) {
        return (*plural).to_string();
    }
    if let Some(stem) = word.strip_suffix('y') {
        if let Some(before) = stem.chars().last() {
            if !matches!(before, 'a' | 'e' | 'i' | 'o' | 'u') {
                return format!("{stem}ies");
            }
        }
    }
    for suffix in ["s", "x", "z", "ch", "sh"] {
        if word.ends_with(suffix) {
            return format!("{word}es");
        }
    }
    format!("{word}s")
}

// ─────────────────────────────────────────────────────────────────────────
// Scope-frame handlers: resources / resource / namespace / scope / controller
// ─────────────────────────────────────────────────────────────────────────

fn handle_resources<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let pairs = kwarg_pairs(args);
    let positionals = positional_args(args);
    let names: Vec<String> = positionals.iter().filter_map(|n| sym_str(n)).collect();
    // C4: any non-literal name in a multi-name `resources :a, :b, …` call
    // escapes the WHOLE declaration once, rather than guessing.
    if names.len() != positionals.len() || names.is_empty() {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    for name in &names {
        handle_one_resources(name, &pairs, block_body, w);
    }
}

fn handle_one_resources<'a>(
    name: &str,
    pairs: &[(String, &Node)],
    block_body: Option<&'a Node>,
    w: &mut Walker<'a>,
) {
    // B3: `module:` wraps an implicit `Scope` frame (so `controller_prefix`
    // picks it up before the resource's own controller is resolved).
    let module_kwarg = kwarg(pairs, "module").and_then(sym_or_str);
    let wrapped = module_kwarg.is_some();
    if let Some(m) = &module_kwarg {
        w.stack.push(Frame::Scope {
            as_prefix: None,
            module: Some(m.clone()),
            controller: None,
        });
    }

    let cp = controller_prefix(&w.stack);
    let as_override = kwarg(pairs, "as").and_then(sym_or_str);
    let (coll_label, sing_label) = match &as_override {
        Some(av) => (av.clone(), singularize_local(av)),
        None => (name.to_string(), singularize_local(name)),
    };
    let member_token = sing_label;
    // v3 §A: `singularize(name) == name` → the `_index` collection-name
    // rule (Rails `Mapper::Resource#collection_name`); the grammatical
    // check runs against the ORIGINAL name, the token TEXT uses the
    // `as:`-overridden label.
    let collection_token = if singularize_local(name) == name {
        format!("{coll_label}_index")
    } else {
        coll_label
    };
    let controller = match kwarg(pairs, "controller").and_then(sym_or_str) {
        Some(c) => apply_controller_override(&c, &cp),
        None => format!("{cp}{name}"),
    };
    let shallow = kwarg(pairs, "shallow").and_then(bool_lit).unwrap_or(false);

    w.stack.push(Frame::Resources {
        collection_token,
        member_token,
        shallow,
        controller,
    });
    let idx = w.stack.len() - 1;

    if let Some(body) = block_body {
        walk_stmt(body, w);
    }

    // C3: canonical actions emit AFTER the block body (block-declared
    // custom routes register first; canonicals shadow-skip on collision).
    let only = kwarg(pairs, "only");
    let except = kwarg(pairs, "except");
    let allowed = allowed_actions(only, except, CANONICAL_ACTIONS);
    emit_canonical_resources(&allowed, idx, w);

    w.stack.pop();
    if wrapped {
        w.stack.pop();
    }
}

fn handle_resource<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let pairs = kwarg_pairs(args);
    let positionals = positional_args(args);
    let names: Vec<String> = positionals.iter().filter_map(|n| sym_str(n)).collect();
    if names.len() != positionals.len() || names.is_empty() {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    for name in &names {
        handle_one_resource(name, &pairs, block_body, w);
    }
}

fn handle_one_resource<'a>(
    name: &str,
    pairs: &[(String, &Node)],
    block_body: Option<&'a Node>,
    w: &mut Walker<'a>,
) {
    let module_kwarg = kwarg(pairs, "module").and_then(sym_or_str);
    let wrapped = module_kwarg.is_some();
    if let Some(m) = &module_kwarg {
        w.stack.push(Frame::Scope {
            as_prefix: None,
            module: Some(m.clone()),
            controller: None,
        });
    }

    let cp = controller_prefix(&w.stack);
    let as_override = kwarg(pairs, "as").and_then(sym_or_str);
    let token = as_override.unwrap_or_else(|| name.to_string());
    let controller = match kwarg(pairs, "controller").and_then(sym_or_str) {
        Some(c) => apply_controller_override(&c, &cp),
        None => format!("{cp}{}", pluralize_local(name)),
    };

    w.stack.push(Frame::Resource {
        collection_token: token.clone(),
        member_token: token,
        controller,
    });
    let idx = w.stack.len() - 1;

    if let Some(body) = block_body {
        walk_stmt(body, w);
    }

    let only = kwarg(pairs, "only");
    let except = kwarg(pairs, "except");
    let allowed = allowed_actions(only, except, SINGLETON_ACTIONS);
    emit_canonical_resources(&allowed, idx, w);

    w.stack.pop();
    if wrapped {
        w.stack.pop();
    }
}

/// Emit the filtered canonical actions for the `Resources`/`Resource`
/// frame at `idx`. Shared by both (a singleton `Resource`'s
/// `collection_token == member_token`, and its `create` sits at the
/// member stem, not a separate collection stem — see `is_resource`
/// below).
fn emit_canonical_resources(allowed: &[String], idx: usize, w: &mut Walker<'_>) {
    let (collection_token, member_token, controller, is_resource) = match &w.stack[idx] {
        Frame::Resources {
            collection_token,
            member_token,
            controller,
            ..
        } => (
            collection_token.clone(),
            member_token.clone(),
            controller.clone(),
            false,
        ),
        Frame::Resource {
            collection_token,
            member_token,
            controller,
        } => (
            collection_token.clone(),
            member_token.clone(),
            controller.clone(),
            true,
        ),
        _ => return,
    };
    let hp = helper_prefix(&w.stack);
    let parent_member = parent_resource_prefix(&w.stack, idx, true);
    let parent_collection = if is_resource {
        parent_member.clone()
    } else {
        parent_resource_prefix(&w.stack, idx, false)
    };
    let collection_stem = format!("{hp}{parent_collection}{collection_token}");
    let member_stem = format!("{hp}{parent_member}{member_token}");

    for action in allowed {
        match action.as_str() {
            "index" => w.emit(
                Some(collection_stem.clone()),
                RouteVerb::Get,
                controller.clone(),
                "index".to_string(),
                RouteScopeKind::Canonical,
            ),
            "create" => w.emit(
                Some(collection_stem.clone()),
                RouteVerb::Post,
                controller.clone(),
                "create".to_string(),
                RouteScopeKind::Canonical,
            ),
            "new" => w.emit(
                Some(format!("new_{member_stem}")),
                RouteVerb::Get,
                controller.clone(),
                "new".to_string(),
                RouteScopeKind::Canonical,
            ),
            "edit" => w.emit(
                Some(format!("edit_{member_stem}")),
                RouteVerb::Get,
                controller.clone(),
                "edit".to_string(),
                RouteScopeKind::Canonical,
            ),
            "show" => w.emit(
                Some(member_stem.clone()),
                RouteVerb::Get,
                controller.clone(),
                "show".to_string(),
                RouteScopeKind::Canonical,
            ),
            "update" => {
                w.emit(
                    Some(member_stem.clone()),
                    RouteVerb::Patch,
                    controller.clone(),
                    "update".to_string(),
                    RouteScopeKind::Canonical,
                );
                w.emit(
                    Some(member_stem.clone()),
                    RouteVerb::Put,
                    controller.clone(),
                    "update".to_string(),
                    RouteScopeKind::Canonical,
                );
            }
            "destroy" => w.emit(
                Some(member_stem.clone()),
                RouteVerb::Delete,
                controller.clone(),
                "destroy".to_string(),
                RouteScopeKind::Canonical,
            ),
            _ => {}
        }
    }
}

fn handle_namespace<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let positionals = positional_args(args);
    let Some(name) = positionals.first().and_then(|n| sym_or_str(n)) else {
        w.escape(EscapeKind::Dynamic);
        return;
    };
    w.stack.push(Frame::Namespace { name });
    if let Some(body) = block_body {
        walk_stmt(body, w);
    }
    w.stack.pop();
}

fn handle_scope<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let pairs = kwarg_pairs(args);
    // A bare positional String/Symbol first arg is a URL path segment —
    // NO helper-prefix effect (v3 §6 item 5: "no prefix effect"; S1 #12:
    // a bare Symbol is coerced the same way — neither contributes to
    // `helper_prefix`/`controller_prefix`).
    let as_prefix = kwarg(&pairs, "as").and_then(sym_or_str);
    let module = kwarg(&pairs, "module").and_then(sym_or_str);
    let controller = kwarg(&pairs, "controller").and_then(sym_or_str);
    w.stack.push(Frame::Scope {
        as_prefix,
        module,
        controller,
    });
    if let Some(body) = block_body {
        walk_stmt(body, w);
    }
    w.stack.pop();
}

fn handle_controller<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let positionals = positional_args(args);
    let Some(name) = positionals.first().and_then(|n| sym_or_str(n)) else {
        return;
    };
    w.stack.push(Frame::Controller { name });
    if let Some(body) = block_body {
        walk_stmt(body, w);
    }
    w.stack.pop();
}

// ─────────────────────────────────────────────────────────────────────────
// Concerns
// ─────────────────────────────────────────────────────────────────────────

fn handle_concern_def<'a>(args: &[Node], block_body: Option<&'a Node>, w: &mut Walker<'a>) {
    let positionals = positional_args(args);
    let Some(name) = positionals.first().and_then(|n| sym_str(n)) else {
        return;
    };
    if let Some(body) = block_body {
        w.concerns.insert(name, body);
    }
}

/// `concerns :name` / `concerns :a, :b` — replay each stored concern body
/// at the CURRENT (invocation-site) scope stack. Same-file only (every
/// corpus site defines before use); an unknown name is `escaped_other`,
/// never a silent no-op.
fn handle_concerns_replay<'a>(args: &[Node], w: &mut Walker<'a>) {
    let names: Vec<String> = args.iter().filter_map(sym_str).collect();
    if names.len() != args.len() || names.is_empty() {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    for name in &names {
        let body: Option<&'a Node> = w.concerns.get(name).copied();
        match body {
            Some(body) => walk_stmt(body, w),
            None => w.escape(EscapeKind::Other(format!("concern:{name}"))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Verb calls
// ─────────────────────────────────────────────────────────────────────────

fn handle_verb(verb: RouteVerb, args: &[Node], w: &mut Walker<'_>) {
    resolve_and_emit(&[verb], args, w);
}

fn handle_match(args: &[Node], w: &mut Walker<'_>) {
    let pairs = kwarg_pairs(args);
    match kwarg(&pairs, "via") {
        Some(Node::Sym(s)) if s.name.to_string_lossy() == "all" => {
            w.escape(EscapeKind::ViaAll);
        }
        Some(Node::Array(a)) => {
            let verbs: Vec<RouteVerb> = a
                .elements
                .iter()
                .filter_map(|e| sym_or_str(e).and_then(|s| RouteVerb::parse(&s)))
                .collect();
            if verbs.is_empty() || verbs.len() != a.elements.len() {
                w.escape(EscapeKind::Dynamic);
                return;
            }
            resolve_and_emit(&verbs, args, w);
        }
        Some(other) => match sym_or_str(other).and_then(|s| RouteVerb::parse(&s)) {
            Some(v) => resolve_and_emit(&[v], args, w),
            None => w.escape(EscapeKind::Dynamic),
        },
        // `match` with no `via:` defaults to GET (Rails' documented
        // default when the verb is unspecified).
        None => resolve_and_emit(&[RouteVerb::Get], args, w),
    }
}

fn handle_root(args: &[Node], w: &mut Walker<'_>) {
    let pairs = kwarg_pairs(args);
    let stem = kwarg(&pairs, "as").and_then(sym_or_str);
    if kwarg(&pairs, "as").is_some() && stem.is_none() {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    let stem = stem.unwrap_or_else(|| "root".to_string());
    let Some(to_node) = kwarg(&pairs, "to") else {
        w.escape(EscapeKind::Dynamic);
        return;
    };
    let Some(to_val) = sym_or_str(to_node) else {
        w.escape(EscapeKind::Dynamic);
        return;
    };
    let Some((ctrl, action)) = split_ctrl_action(&to_val) else {
        w.escape(EscapeKind::Dynamic);
        return;
    };
    let cp = controller_prefix(&w.stack);
    let controller = apply_controller_override(ctrl, &cp);
    w.emit(
        Some(stem),
        RouteVerb::Get,
        controller,
        action.to_string(),
        RouteScopeKind::Standalone,
    );
}

/// A bare identifier token usable as an action-seg for member/collection
/// stems (v2 §3: "a bare-identifier String is stem-equivalent to the
/// symbol form"). A slash/dash-bearing String is deliberately NOT mapped
/// here (that is [`path_auto_name`]'s job, reserved for the standalone
/// and B4-nested resolvers).
fn bare_action_token(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        Node::Str(s) => {
            let v = s.value.to_string_lossy();
            if is_bare_ident(&v) { Some(v) } else { None }
        }
        _ => None,
    }
}

/// Dispatch one verb-call declaration (already split per-verb by the
/// `match … via:` caller) to the right resolver based on the CURRENT
/// scope stack: inside a `member`/`collection` block or `on:` kwarg →
/// [`resolve_member_collection`]; naked inside a bare `Resources`/
/// `Resource` body → [`resolve_naked_in_resource`] (B4); otherwise →
/// [`resolve_standalone`].
fn resolve_and_emit(verbs: &[RouteVerb], args: &[Node], w: &mut Walker<'_>) {
    let pairs = kwarg_pairs(args);
    let positionals = positional_args(args);
    let fat_arrow = fat_arrow_pair(args);

    // Redirect / proc targets escape before any resolution attempt.
    let target_node = kwarg(&pairs, "to").or_else(|| fat_arrow.map(|(_, v)| v));
    if let Some(t) = target_node {
        if is_redirect_call(t) {
            for _ in verbs {
                w.escape(EscapeKind::Redirect);
            }
            return;
        }
        if is_proc_call(t) {
            for _ in verbs {
                w.escape(EscapeKind::Proc);
            }
            return;
        }
    }

    let resource_idx = innermost_resource_idx(&w.stack);
    let on_kwarg = kwarg(&pairs, "on").and_then(sym_or_str);
    let top_is_member = matches!(w.stack.last(), Some(Frame::Member));
    let top_is_collection = matches!(w.stack.last(), Some(Frame::Collection));

    for &verb in verbs {
        match resource_idx {
            Some(idx) => {
                let wrapped_style = if top_is_member || on_kwarg.as_deref() == Some("member") {
                    Some(RouteScopeKind::Member)
                } else if top_is_collection || on_kwarg.as_deref() == Some("collection") {
                    Some(RouteScopeKind::Collection)
                } else {
                    None
                };
                match wrapped_style {
                    Some(style) => {
                        resolve_member_collection(
                            verb,
                            style,
                            idx,
                            &positionals,
                            &pairs,
                            fat_arrow,
                            w,
                        );
                    }
                    None => {
                        resolve_naked_in_resource(verb, idx, &positionals, &pairs, fat_arrow, w);
                    }
                }
            }
            None => resolve_standalone(verb, &positionals, &pairs, fat_arrow, w),
        }
    }
}

/// `true` when every route-relevant kwarg present among `to`/`action`/
/// `controller`/`as` (plus the fat-arrow value, if any) is literal.
/// Non-literal → the whole declaration is `escaped_dynamic` (C5, and the
/// original v2 name/path/as/controller scope).
fn route_relevant_kwargs_literal(
    pairs: &[(String, &Node)],
    fat_arrow: Option<(&Node, &Node)>,
) -> bool {
    for key in ["to", "action", "controller", "as"] {
        if let Some(v) = kwarg(pairs, key) {
            if sym_or_str(v).is_none() {
                return false;
            }
        }
    }
    if let Some((_, v)) = fat_arrow {
        if sym_or_str(v).is_none() {
            return false;
        }
    }
    true
}

/// Member/collection verb calls (block form OR `on:` kwarg) — v2 §3 +
/// B2 (`controller:` kwarg ranks with `to:`/fat-arrow).
fn resolve_member_collection(
    verb: RouteVerb,
    style: RouteScopeKind,
    idx: usize,
    positionals: &[&Node],
    pairs: &[(String, &Node)],
    fat_arrow: Option<(&Node, &Node)>,
    w: &mut Walker<'_>,
) {
    if !route_relevant_kwargs_literal(pairs, fat_arrow) {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    let member_style = matches!(style, RouteScopeKind::Member);
    let (collection_token, member_token, res_controller) = match &w.stack[idx] {
        Frame::Resources {
            collection_token,
            member_token,
            controller,
            ..
        }
        | Frame::Resource {
            collection_token,
            member_token,
            controller,
        } => (
            collection_token.clone(),
            member_token.clone(),
            controller.clone(),
        ),
        _ => return,
    };
    let token = if member_style {
        member_token
    } else {
        collection_token
    };
    let hp = helper_prefix(&w.stack);
    let parent = parent_resource_prefix(&w.stack, idx, member_style);
    let cp = controller_prefix(&w.stack);

    let action_seg_source = positionals
        .first()
        .copied()
        .or_else(|| fat_arrow.map(|(k, _)| k));
    let raw_action_seg = action_seg_source.and_then(bare_action_token);

    let controller_kwarg = kwarg(pairs, "controller").and_then(sym_or_str);
    let to_val = kwarg(pairs, "to")
        .and_then(sym_or_str)
        .or_else(|| fat_arrow.and_then(|(_, v)| sym_or_str(v)));

    let (controller, action): (String, String) = if let Some(cv) = &controller_kwarg {
        let ctrl = apply_controller_override(cv, &cp);
        let act = kwarg(pairs, "action")
            .and_then(sym_or_str)
            .or_else(|| to_val.clone())
            .or_else(|| raw_action_seg.clone());
        match act {
            Some(a) => (ctrl, a),
            None => {
                w.escape(EscapeKind::Dynamic);
                return;
            }
        }
    } else if let Some(tv) = &to_val {
        match split_ctrl_action(tv) {
            Some((c, a)) => (apply_controller_override(c, &cp), a.to_string()),
            None => (res_controller, tv.clone()),
        }
    } else if let Some(av) = kwarg(pairs, "action").and_then(sym_or_str) {
        (res_controller, av)
    } else if let Some(seg) = &raw_action_seg {
        (res_controller, seg.clone())
    } else {
        w.escape(EscapeKind::Dynamic);
        return;
    };

    let as_val = kwarg(pairs, "as").and_then(sym_or_str);
    let stem = if let Some(a) = as_val {
        if a.is_empty() {
            Some(format!("{hp}{parent}{token}"))
        } else {
            Some(format!("{a}_{hp}{parent}{token}"))
        }
    } else {
        match &raw_action_seg {
            Some(seg) if !seg.is_empty() => Some(format!("{seg}_{hp}{parent}{token}")),
            _ => None,
        }
    };

    w.emit(stem, verb, controller, action, style);
}

/// A naked verb call directly inside a `Resources`/`Resource` body (no
/// `member`/`collection` wrapper, no `on:`). B4: a singleton `Resource`
/// stays Member-style via the standard (action-first) formula; a plural
/// `Resources` is NESTED — parent-first ordering, `RouteScope::Member`.
fn resolve_naked_in_resource(
    verb: RouteVerb,
    idx: usize,
    positionals: &[&Node],
    pairs: &[(String, &Node)],
    fat_arrow: Option<(&Node, &Node)>,
    w: &mut Walker<'_>,
) {
    let Frame::Resources {
        member_token,
        collection_token,
        controller: res_controller,
        ..
    } = &w.stack[idx]
    else {
        // Resource (singleton): standard Member-style formula.
        resolve_member_collection(
            verb,
            RouteScopeKind::Member,
            idx,
            positionals,
            pairs,
            fat_arrow,
            w,
        );
        return;
    };
    let member_token = member_token.clone();
    let collection_token = collection_token.clone();
    let res_controller = res_controller.clone();

    if !route_relevant_kwargs_literal(pairs, fat_arrow) {
        w.escape(EscapeKind::Dynamic);
        return;
    }

    let hp = helper_prefix(&w.stack);
    let parent_incl_this = format!(
        "{}{member_token}",
        parent_resource_prefix(&w.stack, idx, true)
    );
    let cp = controller_prefix(&w.stack);

    let controller_kwarg = kwarg(pairs, "controller").and_then(sym_or_str);
    let to_val = kwarg(pairs, "to")
        .and_then(sym_or_str)
        .or_else(|| fat_arrow.and_then(|(_, v)| sym_or_str(v)));
    // B4/C1: the naked-verb path source uses the FULL slash/dash C1
    // mapping (a nested `"/roadmap"` segment names its own action),
    // unlike the bare-identifier-only rule for wrapped member/collection
    // calls.
    let source = positionals
        .first()
        .copied()
        .or_else(|| fat_arrow.map(|(k, _)| k));
    let path_seg = source.and_then(sym_or_str).and_then(|s| path_auto_name(&s));

    let (controller, action): (String, String) = if let Some(cv) = &controller_kwarg {
        let ctrl = apply_controller_override(cv, &cp);
        let act = kwarg(pairs, "action")
            .and_then(sym_or_str)
            .or_else(|| to_val.clone())
            .or_else(|| path_seg.clone());
        match act {
            Some(a) => (ctrl, a),
            None => {
                w.escape(EscapeKind::Dynamic);
                return;
            }
        }
    } else if let Some(tv) = &to_val {
        match split_ctrl_action(tv) {
            Some((c, a)) => (apply_controller_override(c, &cp), a.to_string()),
            None => (res_controller, tv.clone()),
        }
    } else if let Some(av) = kwarg(pairs, "action").and_then(sym_or_str) {
        (res_controller, av)
    } else if let Some(seg) = &path_seg {
        (res_controller, seg.clone())
    } else {
        w.escape(EscapeKind::Dynamic);
        return;
    };

    let as_val = kwarg(pairs, "as").and_then(sym_or_str);
    let action_seg = as_val.or_else(|| path_seg.clone());
    let stem = match action_seg {
        // A bare `/` (or `as: ""`) contributes nothing — the collection
        // candidate falls back to the resource's own collection token
        // (B4/B5).
        Some(seg) if seg.is_empty() => Some(format!(
            "{hp}{}{collection_token}",
            parent_resource_prefix(&w.stack, idx, false)
        )),
        Some(seg) => Some(format!("{hp}{parent_incl_this}_{seg}")),
        None => None,
    };

    w.emit(stem, verb, controller, action, RouteScopeKind::Member);
}

/// Standalone verb calls — not under any `Resources`/`Resource`. v2 §3
/// last bullet + B1/B2.
fn resolve_standalone(
    verb: RouteVerb,
    positionals: &[&Node],
    pairs: &[(String, &Node)],
    fat_arrow: Option<(&Node, &Node)>,
    w: &mut Walker<'_>,
) {
    if !route_relevant_kwargs_literal(pairs, fat_arrow) {
        w.escape(EscapeKind::Dynamic);
        return;
    }
    let cp = controller_prefix(&w.stack);
    let hp = helper_prefix(&w.stack);

    // Rails fallback controller for a bare verb call (explicit action / bare
    // symbol) that has no resource/`controller` frame: the enclosing module
    // scope path acts as the controller. Verified on the corpus:
    // `get "plugin/:id", action: :show_plugin` inside `namespace :admin;
    // namespace :settings` → `Admin::SettingsController#show_plugin`
    // (`app/controllers/admin/settings_controller.rb`), i.e. controller
    // `admin/settings` = the module prefix. `None` only when there is no
    // enclosing module scope at all (a truly context-free top-level verb,
    // absent from this corpus) — that route is recorded stemless, NEVER
    // dumped into `escaped_other` (which is reserved for unknown DSL names).
    let ns_ctrl: Option<String> = {
        let p = cp.trim_end_matches('/');
        (!p.is_empty()).then(|| p.to_string())
    };

    let controller_kwarg = kwarg(pairs, "controller").and_then(sym_or_str);
    let target_val = kwarg(pairs, "to")
        .and_then(sym_or_str)
        .or_else(|| fat_arrow.and_then(|(_, v)| sym_or_str(v)));

    let (controller, action): (String, String) = if let Some(cv) = &controller_kwarg {
        let ctrl = apply_controller_override(cv, &cp);
        let act = kwarg(pairs, "action")
            .and_then(sym_or_str)
            .or_else(|| target_val.clone());
        match act {
            Some(a) => (ctrl, a),
            None => {
                w.escape(EscapeKind::Dynamic);
                return;
            }
        }
    } else if let Some(tv) = &target_val {
        match split_ctrl_action(tv) {
            Some((c, a)) => (apply_controller_override(c, &cp), a.to_string()),
            // ambient controller is a relative name (needs the module prefix);
            // ns_ctrl is already the full module path (used verbatim).
            None => {
                if let Some(amb) = ambient_controller(&w.stack) {
                    (apply_controller_override(&amb, &cp), tv.clone())
                } else if let Some(ns) = &ns_ctrl {
                    (ns.clone(), tv.clone())
                } else {
                    w.emit(
                        None,
                        verb,
                        String::new(),
                        tv.clone(),
                        RouteScopeKind::Standalone,
                    );
                    return;
                }
            }
        }
    } else if let Some(av) = kwarg(pairs, "action").and_then(sym_or_str) {
        if let Some(amb) = ambient_controller(&w.stack) {
            (apply_controller_override(&amb, &cp), av)
        } else if let Some(ns) = &ns_ctrl {
            (ns.clone(), av)
        } else {
            w.emit(None, verb, String::new(), av, RouteScopeKind::Standalone);
            return;
        }
    } else if let Some(sym) = positionals.first().and_then(|n| sym_str(n)) {
        if let Some(amb) = ambient_controller(&w.stack) {
            (apply_controller_override(&amb, &cp), sym)
        } else if let Some(ns) = &ns_ctrl {
            (ns.clone(), sym)
        } else {
            w.emit(None, verb, String::new(), sym, RouteScopeKind::Standalone);
            return;
        }
    } else {
        // Verb call with no target, no action, no symbol — no derivable fact.
        w.emit(
            None,
            verb,
            String::new(),
            String::new(),
            RouteScopeKind::Standalone,
        );
        return;
    };

    let as_val = kwarg(pairs, "as").and_then(sym_or_str);
    let path_source = positionals
        .first()
        .copied()
        .or_else(|| fat_arrow.map(|(k, _)| k));
    let derived = path_source
        .and_then(sym_or_str)
        .and_then(|s| path_auto_name(&s));
    let stem = as_val
        .filter(|a| !a.is_empty())
        .map(|a| format!("{hp}{a}"))
        .or_else(|| {
            derived
                .filter(|d| !d.is_empty())
                .map(|d| format!("{hp}{d}"))
        });

    w.emit(stem, verb, controller, action, RouteScopeKind::Standalone);
}

impl RouteTable {
    /// Lift every stemmed entry into the shared SPO shape: one `RoutesTo`
    /// triple per (stem, verb) — first-declared wins on a C3 conflict,
    /// matching [`Self::resolve`] — and one `RouteScope` triple per
    /// stemmed entry (§2: emitted for EVERY stemmed entry, the closed
    /// 4-value vocab).
    #[must_use]
    pub fn to_triples(&self, namespace: &str) -> Vec<Triple> {
        let mut seen_routes_to: std::collections::HashSet<(String, RouteVerb)> =
            std::collections::HashSet::new();
        let mut seen_scope: std::collections::HashSet<(String, String, String)> =
            std::collections::HashSet::new();
        let mut out = Vec::new();
        for entry in &self.entries {
            let Some(stem) = entry.stem.as_deref() else {
                continue;
            };
            let subject = format!("{namespace}:{stem}");
            if seen_routes_to.insert((stem.to_string(), entry.verb)) {
                out.push(Triple::new(
                    subject.clone(),
                    Predicate::RoutesTo,
                    format!(
                        "{}:{}#{}",
                        entry.verb.as_str(),
                        entry.controller,
                        entry.action
                    ),
                    Provenance::Authoritative,
                ));
            }
            let scope_triple = Triple::new(
                subject,
                Predicate::RouteScope,
                entry.scope.as_str(),
                Provenance::Authoritative,
            );
            let key = (
                scope_triple.s.clone(),
                scope_triple.p.clone(),
                scope_triple.o.clone(),
            );
            if seen_scope.insert(key) {
                out.push(scope_triple);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_routes_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// Write `body` inside `Rails.application.routes.draw do…end` and
    /// scan it. `case` must be unique per test (shared temp-dir scoping).
    fn table_from(case: &str, body: &str) -> (RouteTable, RouteScanReport) {
        let root = scratch_dir(case);
        let cfg = root.join("config");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(
            cfg.join("routes.rb"),
            format!("Rails.application.routes.draw do\n{body}\nend\n"),
        )
        .unwrap();
        let result = extract_routes_with_report(&root, "openproject");
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn entry<'t>(t: &'t RouteTable, stem: &str, verb: RouteVerb) -> Option<&'t RouteEntry> {
        t.entries
            .iter()
            .find(|e| e.stem.as_deref() == Some(stem) && e.verb == verb)
    }

    /// F(1) — canonical seven incl. patch+put double; `only:`/`except:`.
    #[test]
    fn canonical_seven_with_only_except_and_update_double_verb() {
        let (t, _r) = table_from("canon", "  resources :foos\n");
        assert_eq!(entry(&t, "foos", RouteVerb::Get).unwrap().action, "index");
        assert_eq!(entry(&t, "foos", RouteVerb::Post).unwrap().action, "create");
        assert_eq!(entry(&t, "new_foo", RouteVerb::Get).unwrap().action, "new");
        assert_eq!(
            entry(&t, "edit_foo", RouteVerb::Get).unwrap().action,
            "edit"
        );
        assert_eq!(entry(&t, "foo", RouteVerb::Get).unwrap().action, "show");
        assert_eq!(entry(&t, "foo", RouteVerb::Patch).unwrap().action, "update");
        assert_eq!(entry(&t, "foo", RouteVerb::Put).unwrap().action, "update");
        assert_eq!(
            entry(&t, "foo", RouteVerb::Delete).unwrap().action,
            "destroy"
        );
        for e in &t.entries {
            assert_eq!(e.controller, "foos");
            assert_eq!(e.scope, RouteScopeKind::Canonical);
        }

        let (t2, _) = table_from("canon_only", "  resources :foos, only: %i[index show]\n");
        assert_eq!(t2.entries.len(), 2);
        assert!(entry(&t2, "foos", RouteVerb::Get).is_some());
        assert!(entry(&t2, "foo", RouteVerb::Get).is_some());

        let (t3, _) = table_from("canon_except", "  resources :foos, except: :show\n");
        assert!(entry(&t3, "foo", RouteVerb::Get).is_none());
        assert!(entry(&t3, "foos", RouteVerb::Get).is_some());
    }

    /// F(2) — member/collection blocks; `on:` kwargs; naked-verb style
    /// split (Resource → member, Resources → B4-nested).
    #[test]
    fn member_collection_blocks_on_kwarg_and_naked_verb_styles() {
        let (t, _) = table_from(
            "mc",
            "  resources :foos do\n    member do\n      get :reset_dialog\n    end\n    collection do\n      get :dialog\n    end\n    get :on_member, on: :member\n  end\n",
        );
        assert_eq!(
            entry(&t, "reset_dialog_foo", RouteVerb::Get)
                .unwrap()
                .action,
            "reset_dialog"
        );
        assert_eq!(
            entry(&t, "dialog_foos", RouteVerb::Get).unwrap().action,
            "dialog"
        );
        assert_eq!(
            entry(&t, "on_member_foo", RouteVerb::Get).unwrap().action,
            "on_member"
        );

        let (t2, _) = table_from(
            "naked_resource",
            "  resource :form_configuration do\n    get :reset_dialog\n  end\n",
        );
        assert_eq!(
            entry(&t2, "reset_dialog_form_configuration", RouteVerb::Get)
                .unwrap()
                .action,
            "reset_dialog"
        );

        let (t3, _) = table_from(
            "naked_resources",
            "  resources :projects do\n    get \"/roadmap\" => \"versions#index\"\n  end\n",
        );
        let e = entry(&t3, "project_roadmap", RouteVerb::Get).unwrap();
        assert_eq!(e.controller, "versions");
        assert_eq!(e.action, "index");
        assert_eq!(e.scope, RouteScopeKind::Member);
    }

    /// F(3) — `as:` on resources / on a member call / on a string route.
    #[test]
    fn as_override_on_resources_member_and_string_route() {
        // Rails uses the `as:` value VERBATIM for the collection name — it is
        // NOT re-pluralized (Rails guide: `resources :photos, as: 'images'` →
        // `images_path`, never `imageses_path`). So `resources :foos, as: :bar`
        // yields `bar` (index), `new_bar`, `edit_bar`, `bar` (member) — and
        // there is no `bars`. member = singularize(:bar) = :bar, so index and
        // show collide on (bar, get) and dedup per C3. Controller stays `foos`.
        let (t, _) = table_from("as_res", "  resources :foos, as: :bar\n");
        assert!(entry(&t, "bar", RouteVerb::Get).is_some());
        assert!(entry(&t, "new_bar", RouteVerb::Get).is_some());
        assert!(entry(&t, "bars", RouteVerb::Get).is_none());
        assert_eq!(entry(&t, "bar", RouteVerb::Get).unwrap().controller, "foos");

        let (t2, _) = table_from(
            "as_member",
            "  resources :work_packages do\n    member do\n      get \"/new\" => \"work_packages#new\", as: \"new_move\"\n    end\n  end\n",
        );
        assert!(entry(&t2, "new_move_work_package", RouteVerb::Get).is_some());

        let (t3, _) = table_from(
            "as_string",
            "  get \"/api/docs\" => \"api_docs#index\", as: \"api_documentation\"\n",
        );
        assert!(entry(&t3, "api_documentation", RouteVerb::Get).is_some());
    }

    /// F(4) — target forms: `to:"c#a"` / fat-arrow string / fat-arrow
    /// bare symbol / `action:` kwarg + ambient controller /
    /// `controller:`+`action:` kwargs.
    #[test]
    fn target_forms_to_fatarrow_action_and_controller_kwargs() {
        let (t, _) = table_from(
            "targets",
            concat!(
                "  get \"/a\", to: \"ctrl_a#act_a\"\n",
                "  get \"/b\" => \"ctrl_b#act_b\"\n",
                "  get \"/c\" => :act_c, controller: \"ctrl_c\"\n",
                "  scope controller: \"ctrl_d\" do\n",
                "    get \"/d\", action: \"act_d\"\n",
                "  end\n",
                "  get \"/e\", controller: \"ctrl_e\", action: \"act_e\"\n",
            ),
        );
        assert_eq!(entry(&t, "a", RouteVerb::Get).unwrap().controller, "ctrl_a");
        assert_eq!(entry(&t, "b", RouteVerb::Get).unwrap().action, "act_b");
        assert_eq!(entry(&t, "c", RouteVerb::Get).unwrap().controller, "ctrl_c");
        assert_eq!(entry(&t, "c", RouteVerb::Get).unwrap().action, "act_c");
        assert_eq!(entry(&t, "d", RouteVerb::Get).unwrap().controller, "ctrl_d");
        assert_eq!(entry(&t, "e", RouteVerb::Get).unwrap().controller, "ctrl_e");
        assert_eq!(entry(&t, "e", RouteVerb::Get).unwrap().action, "act_e");
    }

    /// F(5) — namespace nesting; `scope module:` / `scope "path"` (no
    /// prefix effect) / `scope :sym` / `scope controller:` /
    /// `controller :x do`.
    #[test]
    fn namespace_and_scope_variants() {
        let (t, _) = table_from(
            "ns",
            "  namespace :admin do\n    resources :widgets, only: %i[index]\n  end\n",
        );
        let e = entry(&t, "admin_widgets", RouteVerb::Get).unwrap();
        assert_eq!(e.controller, "admin/widgets");

        let (t2, _) = table_from(
            "scope_module",
            "  scope module: \"internal\" do\n    get \"/x\", action: \"y\", controller: \"z\"\n  end\n",
        );
        // module: only affects controller_prefix, not helper_prefix.
        assert!(entry(&t2, "x", RouteVerb::Get).is_some());

        let (t3, _) = table_from(
            "scope_path",
            "  scope \"admin\" do\n    get \"/y\", to: \"a#b\"\n  end\n",
        );
        // Bare positional path string on `scope` has NO prefix effect.
        assert!(entry(&t3, "y", RouteVerb::Get).is_some());

        let (t4, _) = table_from(
            "scope_sym",
            "  scope :notifications do\n    get \"/z\", to: \"a#b\"\n  end\n",
        );
        assert!(entry(&t4, "z", RouteVerb::Get).is_some());

        let (t5, _) = table_from(
            "scope_ctrl",
            "  scope controller: \"sys\" do\n    get \"/w\", action: \"ping\"\n  end\n",
        );
        assert_eq!(entry(&t5, "w", RouteVerb::Get).unwrap().controller, "sys");

        let (t6, _) = table_from(
            "bare_ctrl",
            "  controller :help do\n    get \"/help\", action: \"index\"\n  end\n",
        );
        assert_eq!(
            entry(&t6, "help", RouteVerb::Get).unwrap().controller,
            "help"
        );
    }

    /// F(6) — nested resources 2-level AND 3-level.
    #[test]
    fn nested_resources_two_and_three_levels() {
        let (t, _) = table_from(
            "nest2",
            "  resources :projects do\n    resources :meetings, only: %i[index]\n  end\n",
        );
        assert!(entry(&t, "project_meetings", RouteVerb::Get).is_some());

        let (t2, _) = table_from(
            "nest3",
            concat!(
                "  resources :projects do\n",
                "    resources :meetings do\n",
                "      resources :agenda_items, controller: \"meeting_agenda_items\" do\n",
                "        resources :outcomes, controller: \"meeting_outcomes\", only: %i[index]\n",
                "      end\n",
                "    end\n",
                "  end\n",
            ),
        );
        assert!(
            entry(&t2, "project_meeting_agenda_item_outcomes", RouteVerb::Get).is_some(),
            "{:?}",
            t2.entries
                .iter()
                .map(|e| e.stem.clone())
                .collect::<Vec<_>>()
        );
    }

    /// F(7) — singular `resource` (six routes, pluralised controller).
    #[test]
    fn singular_resource_six_routes_pluralized_controller() {
        let (t, _) = table_from("singleton", "  resource :overview\n");
        assert!(entry(&t, "overview", RouteVerb::Get).is_some());
        assert!(entry(&t, "overview", RouteVerb::Post).is_some());
        assert!(entry(&t, "overview", RouteVerb::Patch).is_some());
        assert!(entry(&t, "overview", RouteVerb::Put).is_some());
        assert!(entry(&t, "overview", RouteVerb::Delete).is_some());
        assert!(entry(&t, "new_overview", RouteVerb::Get).is_some());
        assert!(entry(&t, "edit_overview", RouteVerb::Get).is_some());
        assert_eq!(t.entries.len(), 7);
        for e in &t.entries {
            assert_eq!(e.controller, "overviews");
        }
    }

    /// F(8) — `shallow: true`: member-style stems drop the shallow
    /// frame's parent contribution; collection/new keep it.
    #[test]
    fn shallow_drops_parent_contribution_for_member_style_only() {
        let (t, _) = table_from(
            "shallow",
            "  resources :projects, shallow: true do\n    resources :categories, only: %i[index show]\n  end\n",
        );
        // Collection-style keeps the parent.
        assert!(entry(&t, "project_categories", RouteVerb::Get).is_some());
        // Member-style DROPS the shallow parent's own contribution.
        assert!(entry(&t, "category", RouteVerb::Get).is_some());
        assert!(entry(&t, "project_category", RouteVerb::Get).is_none());
    }

    /// F(9) — concern define + replay (call form).
    #[test]
    fn concern_define_and_replay() {
        let (t, _) = table_from(
            "concern",
            concat!(
                "  concern :shareable do\n",
                "    member do\n",
                "      post \"resend_invite\" => \"shares#resend_invite\"\n",
                "    end\n",
                "  end\n",
                "  resources :members, only: %i[index] do\n",
                "    concerns :shareable\n",
                "  end\n",
            ),
        );
        let e = entry(&t, "resend_invite_member", RouteVerb::Post).unwrap();
        assert_eq!(e.controller, "shares");
        assert_eq!(e.action, "resend_invite");
    }

    /// F(10) — `match via:` expansion; `via: :all` escape.
    #[test]
    fn match_via_expansion_and_via_all_escape() {
        let (t, r) = table_from(
            "via",
            concat!(
                "  match \"/auth/:provider/callback\", to: \"a#b\", as: \"auth_cb\", via: %i[get post]\n",
                "  match \"/gone\", to: proc { [410] }, via: :all\n",
            ),
        );
        assert!(entry(&t, "auth_cb", RouteVerb::Get).is_some());
        assert!(entry(&t, "auth_cb", RouteVerb::Post).is_some());
        assert_eq!(r.escaped_via_all, 1);
    }

    /// F(11) — escapes: redirect (string + block form) / mount (both
    /// syntaxes) / proc / `.each` dynamic / an `if`-wrapped route is NOT
    /// escaped (parsed through normally).
    #[test]
    fn escapes_redirect_mount_proc_dynamic_and_if_transparency() {
        let (t, r) = table_from(
            "escapes",
            concat!(
                "  get \"/issues\" => redirect(\"/work_packages\")\n",
                "  get \"/wp/*rest\" => redirect { |params, req| \"x\" }\n",
                "  mount SomeEngine, at: \"/eng\"\n",
                "  mount OtherApi => \"/other\"\n",
                "  match \"/blocked\", to: proc { [410, {}, [\"\"]] }, via: :all\n",
                "  %w[a b].each do |name|\n",
                "    get name, to: \"x#y\"\n",
                "  end\n",
                "  if true\n",
                "    get \"/guarded\", to: \"guard#show\"\n",
                "  end\n",
            ),
        );
        assert_eq!(r.escaped_redirects, 2);
        assert_eq!(r.escaped_mounts, 2);
        assert_eq!(r.escaped_via_all, 1);
        // the if-guarded route IS parsed through — not escaped, not lost.
        assert!(entry(&t, "guarded", RouteVerb::Get).is_some());
    }

    /// F(12) — inflection: statuses→status, activities→activity,
    /// news→news, queries→query; pluralize: overview→overviews,
    /// category→categories.
    #[test]
    fn inflection_singularize_and_pluralize() {
        assert_eq!(singularize_local("statuses"), "status");
        assert_eq!(singularize_local("activities"), "activity");
        assert_eq!(singularize_local("news"), "news");
        assert_eq!(singularize_local("queries"), "query");
        assert_eq!(pluralize_local("overview"), "overviews");
        assert_eq!(pluralize_local("category"), "categories");

        // `resources :news` → the `_index` collection-name rule (v3 §A):
        // singularize("news") == "news" (irregular), so the collection
        // stem gets the `_index` suffix.
        let (t, _) = table_from("news_index", "  resources :news, only: %i[index show]\n");
        assert!(entry(&t, "news_index", RouteVerb::Get).is_some());
        assert!(entry(&t, "news", RouteVerb::Get).is_some());
    }

    /// F(13) — `resolve()` joint incl. an `ActionVerb` → `RouteVerb`
    /// conversion; a `to:` override where the target action differs from
    /// the symbol.
    #[test]
    fn resolve_joint_and_action_verb_conversion() {
        let (t, _) = table_from(
            "resolve",
            "  resources :foos do\n    member do\n      get :parent_page, to: \"foos#edit_parent_page\"\n    end\n  end\n",
        );
        let e = t.resolve("parent_page_foo", RouteVerb::Get).unwrap();
        assert_eq!(e.action, "edit_parent_page");
        assert!(t.resolve("parent_page_foo", RouteVerb::Post).is_none());
        assert!(t.resolve("does_not_exist", RouteVerb::Get).is_none());

        assert_eq!(RouteVerb::from(ActionVerb::Post), RouteVerb::Post);
        assert_eq!(RouteVerb::from(ActionVerb::Put), RouteVerb::Put);
        assert_eq!(RouteVerb::from(ActionVerb::Patch), RouteVerb::Patch);
        assert_eq!(RouteVerb::from(ActionVerb::Delete), RouteVerb::Delete);
    }

    /// F(14) — a no-stem string route is recorded (entry pushed, no
    /// triple), and counted.
    #[test]
    fn no_stem_string_route_is_recorded_not_tripled() {
        let (t, r) = table_from(
            "nostem",
            "  get \"/login/:stage/:secret\", to: \"account#stage_success\"\n",
        );
        assert_eq!(t.entries.len(), 1);
        assert!(t.entries[0].stem.is_none());
        assert_eq!(t.entries[0].controller, "account");
        assert_eq!(r.entries_without_stem, 1);
        assert_eq!(r.entries_emitted, 0);
        assert!(t.to_triples("openproject").is_empty());
    }

    /// F(15) — triple shape: `RoutesTo` object
    /// `"<verb>:<ctrl>#<action>"` lowercase verb; `RouteScope` on EVERY
    /// stemmed entry (4 values); both Authoritative;
    /// `Predicate::from_str(as_str)` roundtrip for this arm's two variants.
    #[test]
    fn triple_shape_and_predicate_roundtrip() {
        let (t, _) = table_from("triples", "  get \"/foo\", to: \"foos#index\"\n");
        let triples = t.to_triples("openproject");
        let routes_to = triples.iter().find(|tr| tr.p == "routes_to").unwrap();
        assert_eq!(routes_to.s, "openproject:foo");
        assert_eq!(routes_to.o, "get:foos#index");
        let (f, c) = Provenance::Authoritative.truth();
        assert_eq!((routes_to.f, routes_to.c), (f, c));
        let scope = triples.iter().find(|tr| tr.p == "route_scope").unwrap();
        assert_eq!(scope.s, "openproject:foo");
        assert_eq!(scope.o, "standalone");

        assert_eq!(Predicate::from_str("routes_to"), Some(Predicate::RoutesTo));
        assert_eq!(
            Predicate::from_str("route_scope"),
            Some(Predicate::RouteScope)
        );
        assert_eq!(
            Predicate::from_str(Predicate::RoutesTo.as_str()),
            Some(Predicate::RoutesTo)
        );
        assert_eq!(
            Predicate::from_str(Predicate::RouteScope.as_str()),
            Some(Predicate::RouteScope)
        );
        // This arm proves only its own two variants roundtrip — it must NOT
        // re-assert the global `Predicate::ALL.len()`. A cross-crate re-assertion
        // of a shared count is the "monitor N pins" anti-pattern the OGAR
        // hot-plug plug-and-play doctrine retires (`OGAR/.claude/knowledge/
        // hotplug-consumer-migration.md`: "wenn's knallt, dann einmal — nicht
        // 200 Pins monitoren"): vocabulary drift is caught precisely, at the
        // consumer boundary, by classid/capability resolution — not by a global
        // integer every arm duplicates. A predicate added by any other arm
        // (e.g. #76's dock/tab/popup plane) must never trip a routes.rs test.
    }

    /// F(16) — member/collection String first-arg: bare identifier ↔
    /// symbol equivalence; slash-string without `as:` → no stem.
    #[test]
    fn member_collection_string_first_arg_bare_ident_and_slash() {
        let (t, _) = table_from(
            "bare_str",
            "  resources :shares, only: %i[] do\n    member do\n      post \"resend_invite\" => \"shares#resend_invite\"\n    end\n  end\n",
        );
        assert!(entry(&t, "resend_invite_share", RouteVerb::Post).is_some());

        let (t2, r2) = table_from(
            "slash_no_as",
            "  resources :repos, only: %i[] do\n    member do\n      get \"/some/path\", to: \"repos#weird\"\n    end\n  end\n",
        );
        assert_eq!(t2.entries.len(), 1);
        assert!(t2.entries[0].stem.is_none());
        assert_eq!(r2.entries_without_stem, 1);
    }

    /// C3 — a later (stem, verb) collision with a DIFFERENT
    /// controller#action counts a conflict, stays in `entries`, and only
    /// the FIRST resolves / gets a `RoutesTo` triple.
    #[test]
    fn duplicate_stem_conflict_first_wins() {
        let (t, r) = table_from(
            "dup",
            concat!(
                "  get \"/x\", to: \"a#b\", as: \"dup\"\n",
                "  get \"/y\", to: \"c#d\", as: \"dup\"\n",
            ),
        );
        assert_eq!(r.duplicate_stem_conflicts, 1);
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.resolve("dup", RouteVerb::Get).unwrap().controller, "a");
        let triples = t.to_triples("openproject");
        assert_eq!(triples.iter().filter(|tr| tr.p == "routes_to").count(), 1);
    }

    /// The F3 identity: `declared_routes == entries_emitted +
    /// entries_without_stem + all escaped_*`.
    #[test]
    fn declared_routes_identity_holds() {
        let (_t, r) = table_from(
            "identity",
            concat!(
                "  resources :foos, only: %i[index show]\n",
                "  get \"/no_stem/:x\", to: \"a#b\"\n",
                "  get \"/issues\" => redirect(\"/x\")\n",
                "  mount SomeEngine, at: \"/eng\"\n",
                "  match \"/all\", to: \"a#b\", via: :all\n",
                "  totally_unknown_dsl :foo\n",
            ),
        );
        let sum = r.entries_emitted
            + r.entries_without_stem
            + r.escaped_redirects
            + r.escaped_mounts
            + r.escaped_procs
            + r.escaped_via_all
            + r.escaped_dynamic
            + r.escaped_other.len();
        assert_eq!(r.declared_routes, sum, "{r:?}");
    }

    /// §7 / F1 — the corpus drift fuse. Env-gated (`RAILS_CORPUS_SRC`),
    /// self-skipping so CI doesn't require the `OpenProject` checkout.
    /// Assertions per the frozen v3 pre-registered census — narrow-only
    /// after a first green run, per F1's own discipline.
    #[test]
    #[allow(clippy::print_stderr)]
    fn corpus_drift_fuse_against_real_openproject_routes() {
        let Ok(root) = std::env::var("RAILS_CORPUS_SRC") else {
            eprintln!("RAILS_CORPUS_SRC unset; skipping the routes.rb corpus fuse");
            return;
        };
        let (table, report) =
            extract_routes_with_report(std::path::Path::new(&root), "openproject");

        assert_eq!(report.files_scanned, 29, "files_scanned");
        assert_eq!(report.escaped_mounts, 9, "escaped_mounts");
        assert_eq!(report.escaped_via_all, 6, "escaped_via_all");
        // `escaped_dynamic` is the MEASURED count (13), not R3's pre-registration
        // of 2 — that estimate grep-counted the two `.each` loop *sites*, but the
        // faithful walker counts every route *declaration* it conservatively
        // declines rather than emit a wrong fact: the two interpolated `.each`
        // bodies (config `workspace_type`/`revision` loops) plus module routes
        // carrying a non-literal-shaped `action:`/path alongside benign unknown
        // kwargs (`work_package_split_view:`, a `defaults:` hash) in calendar
        // (4), team_planner (4), boards (1), backlogs (1). All are counted, none
        // silently dropped (honest denominator). Tightening their classification
        // so more emit a fact is tracked as follow-up polish, not a fact bug.
        assert_eq!(report.escaped_dynamic, 13, "escaped_dynamic (measured)");
        // `escaped_other` is the unknown-DSL-call allowlist: exactly the
        // Doorkeeper OAuth macro. A known verb must NEVER appear here (that was
        // the pre-fix bug where a bare `get`/`post` with no resolvable controller
        // leaked its verb name in — now resolved via the namespace-path fallback).
        assert_eq!(
            report.escaped_other,
            vec!["use_doorkeeper".to_string()],
            "escaped_other must be exactly the Doorkeeper macro — no verb leakage"
        );
        assert!(
            report.declared_routes >= 974,
            "declared_routes {} below the 974 floor (156 resources + 110 resource + 708 verb-calls)",
            report.declared_routes
        );

        // Spot-checks with a confidently-derivable controller#action.
        let spot: &[(&str, RouteVerb, &str, &str)] = &[
            ("news_index", RouteVerb::Get, "news", "index"),
            ("work_packages", RouteVerb::Get, "work_packages", "index"),
            (
                "hover_card_work_package",
                RouteVerb::Get,
                "work_packages/hover_card",
                "show",
            ),
            ("api_docs", RouteVerb::Get, "api_docs", "index"),
            ("home", RouteVerb::Get, "homescreen", "index"),
            ("show_group", RouteVerb::Get, "groups", "show"),
        ];
        for &(stem, verb, ctrl, action) in spot {
            let e = table
                .resolve(stem, verb)
                .unwrap_or_else(|| panic!("expected a resolvable route for stem `{stem}`"));
            assert_eq!(e.controller, ctrl, "stem `{stem}` controller");
            assert_eq!(e.action, action, "stem `{stem}` action");
        }
        // 3-level nesting (projects > meetings > agenda_items > outcomes).
        // The innermost `resources :outcomes, controller: "meeting_outcomes",
        // except: %i[index show]` EXCLUDES the show/index routes, so the bare
        // `project_meeting_agenda_item_outcome` (show) helper does NOT exist —
        // the walker correctly respects `except:`. The `new` helper does exist
        // and is used in-app (`new_project_meeting_agenda_item_outcome_path`,
        // meeting show_component.rb:186).
        let nested = table
            .resolve("new_project_meeting_agenda_item_outcome", RouteVerb::Get)
            .expect("3-level nested `new` helper must resolve");
        assert_eq!(
            nested.controller, "meeting_outcomes",
            "3-level nesting controller"
        );
        assert_eq!(nested.action, "new", "3-level nesting action");
        // The excepted show route must NOT be emitted.
        assert!(
            table
                .resolve("project_meeting_agenda_item_outcome", RouteVerb::Get)
                .is_none(),
            "excepted show route must not be emitted"
        );
        // The 2nd `root to: "account#login"` (no `as:`) — stem "root".
        assert!(table.resolve("root", RouteVerb::Get).is_some());

        // False-positive guard for B/C1: no emitted stem carries
        // interpolation/brace residue, and the two known `.each`
        // would-be-guesses never leaked a stem.
        for e in &table.entries {
            if let Some(stem) = &e.stem {
                assert!(
                    !stem.contains('#') && !stem.contains('{') && !stem.contains('}'),
                    "stem `{stem}` carries interpolation residue"
                );
                assert_ne!(stem, "workspace_types");
                assert_ne!(stem, "diff_revision");
            }
        }

        // F3 identity, on the real corpus too.
        let sum = report.entries_emitted
            + report.entries_without_stem
            + report.escaped_redirects
            + report.escaped_mounts
            + report.escaped_procs
            + report.escaped_via_all
            + report.escaped_dynamic
            + report.escaped_other.len();
        assert_eq!(report.declared_routes, sum, "{report:?}");

        eprintln!(
            "routes.rb corpus fuse: {} files, {} declared, {} entries ({} without stem), \
             {} duplicate conflicts, escaped: redirects={} mounts={} procs={} via_all={} \
             dynamic={} other={:?}",
            report.files_scanned,
            report.declared_routes,
            table.entries.len(),
            report.entries_without_stem,
            report.duplicate_stem_conflicts,
            report.escaped_redirects,
            report.escaped_mounts,
            report.escaped_procs,
            report.escaped_via_all,
            report.escaped_dynamic,
            report.escaped_other,
        );
    }
}
