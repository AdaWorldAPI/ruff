//! The Rails **menu-DSL region-grammar** harvest — the six-region layout
//! plane, ported from ruff #76 (`WinForms` `Designer.cs` dock/tab-order/popup)
//! to the Rails `Redmine::MenuManager` DSL. See `.claude/knowledge/
//! six-region-layout-port.md` (lance-graph) for the full port rationale.
//!
//! # The model
//!
//! The MENU is the "screen"; each pushed ITEM is a "control": for a
//! `menu.push :item, url, opts` inside `MenuManager.map :menu_name`, the
//! item docks at `menu_name` (raw token — the dock-token → six-region
//! mapping is 100% downstream config, never hardcoded here), its sibling
//! `tab_order` is derived (Rails has no numeric `TabIndex`; §3 below
//! replays declaration order + `first`/`last`/`before`/`after` into an
//! ordinal), and a `parent:` kwarg emits `contains_control` nesting.
//!
//! Reuses the shared closed-vocab predicates minted for the `WinForms` arm
//! (`DockedAt` / `TabOrder` / `ContainsControl`, all
//! [`Provenance::Authoritative`]) — no new predicate, no vocab bump.
//!
//! # AST walk, not a line scanner
//!
//! [`crate::menu`]'s line scanner cannot parse the real corpus shape:
//! multi-line `menu.push` calls (item + url + kwargs spanning 4+ lines are
//! the norm), and module engines register menu items nested arbitrarily
//! deep (`module … class Engine < ::Rails::Engine … register(...) do
//! Redmine::MenuManager.map(:admin_menu) do |menu| … end end end end`).
//! This arm walks `lib-ruby-parser`'s AST: a generic top-level descent
//! ([`walk_top`]) threads through `Begin`/`If`/`IfMod`/`Module`/`Class`/
//! `Def`/`Defs`/`Block` looking for a `MenuManager.map`/bare `menu` block
//! call with a literal `Sym` name; once found, [`walk_menu_body`] walks
//! that block's body specifically for `<x>menu.push` sites (receiver name
//! ending in `"menu"`, mirroring [`crate::menu`]'s heuristic).
//!
//! # The dynamic per-key loop (menus.rb:808, `:project_menu`'s `:settings`
//! children)
//!
//! `OpenProject` builds its settings sub-items from a literal
//! `Hash<Sym, Hash>` local var, iterated with `.each do |key, options|`
//! and pushed via an interpolated `:"settings_#{key}"` symbol. This is
//! genuinely dynamic — no static `Sym` literal names the item — but the
//! HASH's keys and the call's own literal kwargs (`parent: :settings`) ARE
//! static, so [`walk_menu_body`] tracks `<lvar> = { sym: ..., ... }`
//! bindings and expands `<lvar>.each do |k, opts| … menu.push :"prefix#{k}",
//! …, parent: …` once per hash key. This is a real necessity, not
//! gold-plating: without it `:settings`'s `contains_control` children
//! (`settings_general`, `settings_modules`, …) would never resolve.
//!
//! # Honest deltas (§7, do not paper over)
//!
//! 1. `opens_popup` is NOT harvested — Rails' popup binding is a Primer
//!    component render / Angular `OpContextMenuService` registration, not
//!    a clean `ContextMenuStrip =` assignment. Deferred.
//! 2. `bottom_bar` is legitimately empty for `OpenProject` (no footer
//!    region) — a downstream-config fact, not something this arm encodes.
//! 3. **Load-order assumption**: declaration order is approximated as
//!    file-path-sorted + AST-visit (source) order, not Rails' true
//!    initializer/engine `require` order. The one ordering assumption.
//! 4. `content_for :sidebar` is a `left_nav` sub-panel, not a region —
//!    downstream-config concern, irrelevant to this harvester.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lib_ruby_parser::{Node, Parser, ParserOptions};
use ruff_spo_triplet::{
    MenuQuad, Predicate, Provenance, PurposeRole, PurposeRule, RegionFact, Triple, classify_purpose,
};

/// The Rails `purpose`-axis config (the operator's "config over reusable"): the
/// target REST-style `action:` classifies into the shared [`PurposeRole`] vocab
/// via the shared [`classify_purpose`] engine. Rules are existential
/// (`min_hits: 1`) — Rails carries one action token per item. Priority: a
/// list/index surface, then a detail/show, then a create/edit form; a
/// custom/unknown action falls to [`RAILS_PURPOSE_FALLBACK`].
const RAILS_PURPOSE: &[PurposeRule] = &[
    PurposeRule {
        needles: &["index"],
        role: PurposeRole::List,
        min_hits: 1,
    },
    PurposeRule {
        needles: &["show"],
        role: PurposeRole::Detail,
        min_hits: 1,
    },
    PurposeRule {
        needles: &["new", "edit"],
        role: PurposeRole::Form,
        min_hits: 1,
    },
];

/// A menu item whose target action is custom/unknown (a non-REST verb) is a
/// plain navigation trigger — [`PurposeRole::Action`].
const RAILS_PURPOSE_FALLBACK: PurposeRole = PurposeRole::Action;

/// One harvested menu-item region placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionEntry {
    /// The `menu_name` (the region token, pre-config — the six-region
    /// dock-token mapping is downstream config, never hardcoded here).
    pub menu: String,
    /// The pushed item symbol (or the synthesized `"<prefix><key>"` for a
    /// dynamic per-key loop expansion — see the module docs).
    pub item: String,
    /// The `parent:` kwarg, if present. `None` means the item's container
    /// is the menu itself (implicit root).
    pub parent: Option<String>,
    /// The raw declared position directive.
    pub position: Position,
    /// The resolved 0-based sibling ordinal (§3), assigned by the single-
    /// pass Rails `TreeNode` replay in `resolve_group`. Always `Some`
    /// under that model (declaration-order resolution always terminates);
    /// the `Option` is retained so any future regression that fails to
    /// assign an ordinal surfaces as `None` + a non-zero
    /// [`RegionScanReport::unresolved_order`] rather than a silent wrong 0.
    pub tab_order: Option<u32>,
    /// The target `action:` (the `purpose`-axis signal for the menu quad).
    /// `None` when the push has no `action:`; a bare `controller:` then
    /// defaults to Rails' REST-style `index` (see [`Self::to_quad`]).
    pub action: Option<String>,
    /// Whether the push declares a `controller:` target — used to distinguish
    /// "no target at all" from "target with the implicit `index` action".
    pub has_controller: bool,
    /// The `controller:` kwarg's VALUE (e.g. `"/work_packages"`,
    /// `"/admin/settings"`), when STATICALLY resolvable (a `Sym`/`Str`
    /// literal) — the identity-binding arm's raw signal (see
    /// `derive_model_from_controller`). `None` for an absent `controller:`
    /// (mirrors `has_controller == false`) OR a dynamic value (e.g.
    /// `options[:controller]` in the each-loop expansion — `has_controller`
    /// stays `true` there, but there is nothing static to derive from).
    pub controller: Option<String>,
    /// The statically-extractable permission symbols named in the push's `if:`
    /// visibility guard, deduped + sorted for determinism. Empty when the push
    /// has no `if:`, when the guard names no `allowed_*` permission (a bare
    /// `admin?`/`logged?`/`Setting.*` guard), or when every permission argument
    /// is dynamic (a `Hash`/method-call rather than a `Sym` literal — never
    /// fabricated). Both operands of a `||`/`&&` guard contribute, per the
    /// visibility-honest [`Predicate::GuardedByPermission`] semantics.
    pub permissions: Vec<String>,
    /// Path relative to the corpus root, `/`-joined.
    pub file: String,
}

/// A `menu.push` position directive — declaration order (the default),
/// or one of Rails' `first`/`last`/`before`/`after` kwargs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    Append,
    First,
    Last,
    Before(String),
    After(String),
}

/// Conservation-ledger totals for a region scan — same honest-denominator
/// discipline as [`crate::routes::RouteScanReport`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionScanReport {
    /// `config/initializers/menus.rb` + every matched `engine.rb` / lib
    /// menu file found.
    pub files_scanned: usize,
    /// `MenuManager.map`/bare `menu` blocks with a resolvable literal
    /// name — raw occurrence count (NOT deduped by menu name; see
    /// [`Self::menus`] for the distinct set).
    pub map_blocks: usize,
    /// Every resolved [`RegionEntry`] emitted (static pushes + dynamic
    /// per-key loop expansions).
    pub items: usize,
    /// Entries with `parent.is_some()`.
    pub with_parent: usize,
    /// Entries whose declared [`Position`] is not [`Position::Append`].
    pub with_position: usize,
    /// Entries with at least one statically-extractable `if:`-guard permission
    /// symbol (the OQ-GUARD-1 conservation counter for
    /// [`Predicate::GuardedByPermission`]).
    pub with_permission: usize,
    /// Entries left without a `tab_order` after resolution. Structurally 0
    /// under the single-pass Rails `TreeNode` replay (which always
    /// terminates); retained as a regression tripwire — a non-zero value
    /// means a future change stopped assigning an ordinal to some item.
    pub unresolved_order: usize,
    /// Distinct `menu_names` seen, sorted.
    pub menus: Vec<String>,
    // ── the `surfaces_concept` identity-binding 5-bucket ledger (SPEC v2) ──
    /// Entries with NO `controller:` target at all (`has_controller ==
    /// false`) — identity dormant BY DESIGN (settings/help/external-URL
    /// items have no backing model). Distinct from
    /// [`Self::with_dynamic_controller`], which HAS a `controller:` we
    /// simply can't resolve statically.
    pub without_concept: usize,
    /// Entries with a `controller:` kwarg whose value is DYNAMIC (e.g. the
    /// each-loop `controller: options[:controller]`) — `has_controller ==
    /// true` but no static token to derive from. NOT identity-dormant "by
    /// design" (there IS a target); a visible non-emission bucket that
    /// names the next arm (an each-loop expansion) rather than hiding a
    /// dynamic target inside [`Self::without_concept`].
    pub with_dynamic_controller: usize,
    /// Entries whose identity is bound from a DECLARED literal (a Rails
    /// config-row arm, if one is ever added — always 0 today; Rails only
    /// has the derived arm below). Kept for report-shape parity with the
    /// Odoo/C# arms, which DO have a declared source.
    pub with_concept_declared: usize,
    /// Entries whose `controller:`-derived model token matched the real
    /// roster — the honest `OpenProjectExtracted` emission.
    pub with_concept_derived_matched: usize,
    /// Entries whose `controller:`-derived model token did NOT match the
    /// real roster — visible refusal (irregular plurals, a namespaced
    /// controller the `/`-fix doesn't cover, or a genuinely model-less
    /// controller). A low `with_concept_*` fraction overall is correct, not
    /// a failure — most menu items are not resource-CRUD screens.
    pub with_concept_derived_unmatched: usize,
}

/// One collected `menu.push` site, pre-tab_order-resolution. Frontend-local
/// — never exposed; [`extract_regions_with_report`] converts each into a
/// [`RegionEntry`] once §3's two-pass resolution has run.
struct Registration {
    menu: String,
    item: String,
    parent: Option<String>,
    position: Position,
    /// The target `action:` from the push opts (the `purpose`-axis signal).
    /// `None` when unset — a bare `controller:` defaults to Rails' `index`.
    action: Option<String>,
    /// Whether the push carries a `controller:` target (so a missing `action:`
    /// resolves to the REST-style `index` default rather than no signal at all).
    has_controller: bool,
    /// The `controller:` kwarg's value. See [`RegionEntry::controller`].
    controller: Option<String>,
    /// Permission symbols named in the push's `if:` guard (deduped + sorted).
    /// See [`RegionEntry::permissions`].
    permissions: Vec<String>,
    file: String,
}

/// Scan `<root>` for menu-DSL region placements. Thin wrapper over
/// [`extract_regions_with_report`].
#[must_use]
pub fn extract_regions(root: &Path) -> Vec<RegionEntry> {
    extract_regions_with_report(root, "").0
}

/// Like [`extract_regions`] but also returns the [`RegionScanReport`].
///
/// `namespace` is accepted for signature parity with the crate's other
/// `extract_*_with_report(root, namespace)` entry points and is what a
/// caller subsequently passes to [`RegionEntry::to_triples`] —
/// [`RegionEntry`] stays namespace-free (bare menu/item tokens), so it is
/// not consumed inside this scan (mirrors `routes.rs`'s
/// `extract_routes_with_report` discipline).
#[must_use]
pub fn extract_regions_with_report(
    root: &Path,
    _namespace: &str,
) -> (Vec<RegionEntry>, RegionScanReport) {
    let mut regs: Vec<Registration> = Vec::new();
    let mut report = RegionScanReport::default();

    for path in collect_menu_files(root) {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = relative_path(root, &path);
        report.files_scanned += 1;
        walk_menu_file(&src, &rel, &mut regs, &mut report);
    }

    let (tab_orders, unresolved) = resolve_tab_orders(&regs);
    report.unresolved_order = unresolved;

    let entries: Vec<RegionEntry> = regs
        .into_iter()
        .zip(tab_orders)
        .map(|(r, tab_order)| RegionEntry {
            menu: r.menu,
            item: r.item,
            parent: r.parent,
            position: r.position,
            tab_order,
            action: r.action,
            has_controller: r.has_controller,
            controller: r.controller,
            permissions: r.permissions,
            file: r.file,
        })
        .collect();

    report.items = entries.len();
    report.with_parent = entries.iter().filter(|e| e.parent.is_some()).count();
    report.with_position = entries
        .iter()
        .filter(|e| !matches!(e.position, Position::Append))
        .count();
    report.with_permission = entries.iter().filter(|e| !e.permissions.is_empty()).count();
    let mut menus: Vec<String> = entries.iter().map(|e| e.menu.clone()).collect();
    menus.sort();
    menus.dedup();
    report.menus = menus;

    // The `surfaces_concept` identity-binding 5-bucket ledger.
    let roster = crate::schema::model_roster(root);
    for binding in bind_identities(&entries, &roster) {
        match binding {
            IdentityBinding::WithoutConcept => report.without_concept += 1,
            IdentityBinding::DynamicController => report.with_dynamic_controller += 1,
            IdentityBinding::DerivedMatched(_) => report.with_concept_derived_matched += 1,
            IdentityBinding::DerivedUnmatched => report.with_concept_derived_unmatched += 1,
        }
    }

    (entries, report)
}

fn walk_menu_file(
    src: &str,
    file: &str,
    regs: &mut Vec<Registration>,
    report: &mut RegionScanReport,
) {
    let options = ParserOptions {
        buffer_name: file.to_string(),
        ..Default::default()
    };
    let parser = Parser::new(src.as_bytes().to_vec(), options);
    let Some(ast) = parser.do_parse().ast.map(|b| *b) else {
        return;
    };
    let mut w = Walker {
        file: file.to_string(),
        hash_vars: HashMap::new(),
        regs,
        report,
    };
    walk_top(&ast, &mut w);
}

struct Walker<'r> {
    file: String,
    /// `<lvar> = { sym_key: ..., ... }` bindings seen so far in the current
    /// menu block — the each-loop expansion's hash-of-syms tracking.
    hash_vars: HashMap<String, Vec<String>>,
    regs: &'r mut Vec<Registration>,
    report: &'r mut RegionScanReport,
}

/// `<root>/config/initializers/menus.rb` + every matched `engine.rb` / lib
/// menu file, filename-sorted for determinism.
fn collect_menu_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let core = root.join("config/initializers/menus.rb");
    if core.is_file() {
        files.push(core);
    }
    walk_collect(root, &mut files);
    files.sort();
    files.dedup();
    files
}

/// Directories that cannot contain menu-DSL registrations and are safe to
/// prune from the walk (perf + avoids picking up rspec fixtures).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "tmp",
    "log",
    "coverage",
    "public",
    "vendor",
    "spec",
    "test",
    "frontend",
    "docs",
];

/// Every `engine.rb` file, plus every `*.rb` file under a `lib/` directory
/// whose filename contains `"menu"` — the bounded heuristic for "lib menu
/// files" (§4 of the frozen spec).
fn walk_collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && SKIP_DIRS.contains(&name)
            {
                continue;
            }
            walk_collect(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rb") {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if file_name == "engine.rb" {
                out.push(path);
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let is_in_lib = path.components().any(|c| c.as_os_str() == "lib");
            if is_in_lib && stem.contains("menu") {
                out.push(path);
            }
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ─────────────────────────────────────────────────────────────────────────
// §3 — the tab_order resolution algorithm
// ─────────────────────────────────────────────────────────────────────────

/// Resolve `tab_order` for every registration: group by `(menu, parent)`
/// sibling group, then replay each group in declaration order via
/// [`resolve_group`]'s faithful Rails `TreeNode` single pass. Group
/// membership preserves the `regs` slice's own index order (files walked in
/// sorted-path order, each file's AST visited in source order — the
/// "file+line declaration order" approximation the spec names as its one
/// ordering assumption). Returns one `Option<u32>` per `regs` entry (by
/// index) plus the count of entries left without a `tab_order` — always 0
/// under the single-pass model (declaration-order resolution always
/// terminates; the field is retained for report-shape stability and to
/// catch any future regression that fails to assign an ordinal).
fn resolve_tab_orders(regs: &[Registration]) -> (Vec<Option<u32>>, usize) {
    let mut tab_order: Vec<Option<u32>> = vec![None; regs.len()];

    let mut groups: HashMap<(String, Option<String>), Vec<usize>> = HashMap::new();
    for (i, r) in regs.iter().enumerate() {
        groups
            .entry((r.menu.clone(), r.parent.clone()))
            .or_default()
            .push(i);
    }

    for order in groups.into_values() {
        resolve_group(regs, order, &mut tab_order);
    }

    let unresolved = tab_order.iter().filter(|t| t.is_none()).count();
    (tab_order, unresolved)
}

/// Resolve one `(menu, parent)` sibling group in place by a **faithful
/// single-pass replay of Rails `MenuManager::TreeNode`** (`tree_node.rb`):
/// process the registrations in declaration order, mutating a `children`
/// vec exactly as Rails does per push, so the final index IS the
/// `tab_order`. This replaced an earlier phase-separated model (First →
/// Last → Before/After in fixed stages) that diverged from Rails in two
/// code-proven ways: multiple `first:` items are LIFO in Rails (each
/// `prepend` inserts at index 0), not FIFO; and a plain push after a
/// `before:`/`after:` splice lands relative to the *live*
/// `size - last_count` boundary, which a staged model can't reproduce.
/// The single pass matches Rails' one mutating pass exactly, and Rails'
/// at-push-time `exists?` means a forward-referenced or absent anchor
/// falls back to a plain `add` — so there is no unresolvable/cyclic case
/// to detect (declaration-order resolution always terminates).
///
/// Per-push semantics (Rails `mapper.rb` push dispatch + `tree_node.rb`):
/// - `first`  → `prepend`: insert at 0.
/// - `before`/`after` with the anchor **already present**: `add_at` at the
///   anchor's current index (`+1` for `after`); does NOT touch `last_count`.
/// - `before`/`after` with a **missing** anchor → fall through to a plain
///   `add` (Rails checks `exists?` and only then splices).
/// - `last`   → `add_last`: append AND `last_count += 1`.
/// - plain    → `add`: insert at `children.len() - last_count` (just before
///   the trailing `last:` band).
fn resolve_group(regs: &[Registration], order: Vec<usize>, tab_order: &mut [Option<u32>]) {
    let mut children: Vec<usize> = Vec::with_capacity(order.len());
    let mut last_count: usize = 0;

    // A plain `add` inserts just before the trailing `last:` band.
    let plain_pos = |children: &Vec<usize>, last_count: usize| children.len() - last_count;
    // The anchor's current index within `children`, by item name.
    let anchor_pos = |children: &Vec<usize>, anchor: &str| {
        children
            .iter()
            .position(|&x| regs[x].item.as_str() == anchor)
    };

    for i in order {
        match &regs[i].position {
            Position::First => children.insert(0, i),
            Position::Last => {
                children.push(i);
                last_count += 1;
            }
            Position::Before(anchor) => match anchor_pos(&children, anchor) {
                Some(p) => children.insert(p, i),
                None => children.insert(plain_pos(&children, last_count), i),
            },
            Position::After(anchor) => match anchor_pos(&children, anchor) {
                Some(p) => children.insert(p + 1, i),
                None => children.insert(plain_pos(&children, last_count), i),
            },
            Position::Append => children.insert(plain_pos(&children, last_count), i),
        }
    }

    for (idx, id) in children.into_iter().enumerate() {
        tab_order[id] = Some(u32::try_from(idx).unwrap_or(u32::MAX));
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The walker
// ─────────────────────────────────────────────────────────────────────────

/// `Send{recv: MenuManager-ish, method: "map"}` — matched on the innermost
/// `Const` name alone (not the full `Redmine::`/`::Redmine::` scope chain),
/// same discipline as `routes.rs`'s `find_draw_body`.
fn is_menu_map_call(s: &lib_ruby_parser::nodes::Send) -> bool {
    s.method_name == "map"
        && matches!(s.recv.as_deref(), Some(Node::Const(c)) if c.name == "MenuManager")
}

/// The module `menu :name do |menu| … end` alternate form (§4 of the
/// frozen spec) — a bare (receiverless) `menu` call taking a block.
fn is_bare_menu_block_call(s: &lib_ruby_parser::nodes::Send) -> bool {
    s.recv.is_none() && s.method_name == "menu"
}

/// The map/menu block's literal `Sym`/`Str` name argument, or `None` for a
/// dynamic name (e.g. `lib/redmine/plugin.rb`'s `Plugin#menu` helper,
/// which forwards a method parameter — genuinely unresolvable statically).
fn extract_map_menu_name(s: &lib_ruby_parser::nodes::Send) -> Option<String> {
    positional_args(&s.args).first().and_then(|n| sym_or_str(n))
}

/// Generic top-level descent: find `MenuManager.map`/bare `menu` block
/// calls with a literal name anywhere in the file, however deeply nested
/// (module engines wrap them in `class Engine < ::Rails::Engine …
/// register(...) do … end … end`). Transparent through `Begin`/`If`/
/// `IfMod`/`Module`/`Class`/`Def`/`Defs`/any non-matching `Block` body —
/// this is deliberately broader than `routes.rs`'s targeted DSL-wrapper
/// dispatch because menu registrations are NOT confined to a fixed set of
/// wrapper method names (`register`, `initializer`, arbitrary `Engine`
/// class bodies all legitimately carry them).
fn walk_top(node: &Node, w: &mut Walker<'_>) {
    match node {
        Node::Begin(b) => {
            for stmt in &b.statements {
                walk_top(stmt, w);
            }
        }
        Node::If(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_top(t, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_top(f, w);
            }
        }
        Node::IfMod(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_top(t, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_top(f, w);
            }
        }
        Node::Module(m) => {
            if let Some(body) = &m.body {
                walk_top(body, w);
            }
        }
        Node::Class(c) => {
            if let Some(body) = &c.body {
                walk_top(body, w);
            }
        }
        Node::Def(d) => {
            if let Some(body) = d.body.as_deref() {
                walk_top(body, w);
            }
        }
        Node::Defs(d) => {
            if let Some(body) = d.body.as_deref() {
                walk_top(body, w);
            }
        }
        Node::Block(blk) => {
            if let Node::Send(s) = blk.call.as_ref()
                && (is_menu_map_call(s) || is_bare_menu_block_call(s))
                && let Some(name) = extract_map_menu_name(s)
            {
                w.report.map_blocks += 1;
                if let Some(body) = blk.body.as_deref() {
                    walk_menu_body(body, &name, w);
                }
                return;
            }
            if let Some(body) = blk.body.as_deref() {
                walk_top(body, w);
            }
        }
        _ => {}
    }
}

/// Walk a matched map/menu block's body. Transparent through `Begin`/`If`/
/// `IfMod` (declarations may be conditionally guarded); recognises three
/// statement shapes: a direct `<x>menu.push` call, a `<lvar> = {sym: …}`
/// hash-of-syms binding (tracked for the each-loop expansion below), and
/// a `<hash_lvar>.each do |k, opts| … end` block (expanded per hash key).
fn walk_menu_body(node: &Node, menu: &str, w: &mut Walker<'_>) {
    match node {
        Node::Begin(b) => {
            for stmt in &b.statements {
                walk_menu_body(stmt, menu, w);
            }
        }
        Node::If(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_menu_body(t, menu, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_menu_body(f, menu, w);
            }
        }
        Node::IfMod(i) => {
            if let Some(t) = i.if_true.as_deref() {
                walk_menu_body(t, menu, w);
            }
            if let Some(f) = i.if_false.as_deref() {
                walk_menu_body(f, menu, w);
            }
        }
        Node::Lvasgn(a) => {
            if let Some(Node::Hash(h)) = a.value.as_deref()
                && let Some(keys) = hash_sym_keys(h)
            {
                w.hash_vars.insert(a.name.clone(), keys);
            }
        }
        Node::Send(s) if is_menu_push_call(s) => {
            handle_push(menu, None, &s.args, w);
        }
        Node::Block(blk) => {
            if let Node::Send(s) = blk.call.as_ref()
                && s.method_name == "each"
                && let Some(Node::Lvar(lv)) = s.recv.as_deref()
                && let Some(keys) = w.hash_vars.get(&lv.name).cloned()
                && let Some(body) = blk.body.as_deref()
            {
                expand_each_loop(menu, &keys, body, w);
            }
        }
        _ => {}
    }
}

/// `<x>menu.push(...)` — receiver name ends in `"menu"` (mirrors
/// [`crate::menu`]'s `menu_push_call` heuristic, at the AST level instead
/// of text scanning), bounding this away from unrelated `.push` calls.
fn is_menu_push_call(s: &lib_ruby_parser::nodes::Send) -> bool {
    s.method_name == "push"
        && matches!(s.recv.as_deref(), Some(Node::Lvar(l)) if l.name.ends_with("menu"))
}

/// Every pair's key as a literal `Sym`, in declaration order — `None` if
/// ANY pair has a non-`Sym` key (a genuine hash-of-syms is the only shape
/// the each-loop expansion understands; anything else is left alone).
fn hash_sym_keys(h: &lib_ruby_parser::nodes::Hash) -> Option<Vec<String>> {
    let mut keys = Vec::with_capacity(h.pairs.len());
    for pair_node in &h.pairs {
        let Node::Pair(p) = pair_node else {
            return None;
        };
        let Node::Sym(s) = p.key.as_ref() else {
            return None;
        };
        keys.push(s.name.to_string_lossy());
    }
    if keys.is_empty() { None } else { Some(keys) }
}

/// Record one `menu.push` site as a [`Registration`]. `item_override` is
/// `Some` for an each-loop expansion (the item name isn't in the call's
/// own args at all — it's synthesized from the hash key); `None` for a
/// direct push, where the item is the call's first positional `Sym`/`Str`.
/// A non-literal item name (dynamic target, e.g. `main_item.menu_identifier`
/// in `wiki_menu_helper.rb`) is silently skipped — genuinely unresolvable
/// statically, not a guess.
fn handle_push(menu: &str, item_override: Option<String>, args: &[Node], w: &mut Walker<'_>) {
    let item = match item_override {
        Some(s) => s,
        None => {
            let positionals = positional_args(args);
            match positionals.first().and_then(|n| sym_or_str(n)) {
                Some(s) => s,
                None => return,
            }
        }
    };
    let pairs = kwarg_pairs(args);
    let parent = kwarg(&pairs, "parent").and_then(sym_or_str);
    let position = extract_position(&pairs);
    // `kwarg_pairs` flattens the positional `{controller:, action:}` options
    // hash and any trailing kwargs into one vec, so `action`/`controller` are
    // plain key lookups (distinct from the routes arm, which reads routes.rb).
    let action = kwarg(&pairs, "action").and_then(sym_or_str);
    let controller_kwarg = kwarg(&pairs, "controller");
    let has_controller = controller_kwarg.is_some();
    // The literal VALUE, when statically resolvable (a Sym/Str) — `None` for
    // an absent `controller:` OR a dynamic value (e.g. `options[:controller]`
    // in the each-loop expansion), same "never fabricated" discipline as
    // every other static-only extraction in this arm.
    let controller = controller_kwarg.and_then(sym_or_str);
    let permissions = kwarg(&pairs, "if")
        .map(extract_guard_permissions)
        .unwrap_or_default();
    w.regs.push(Registration {
        menu: menu.to_string(),
        item,
        parent,
        position,
        action,
        has_controller,
        controller,
        permissions,
        file: w.file.clone(),
    });
}

/// `first:`/`last:`/`before:`/`after:` kwargs → [`Position`]. Checked in
/// that priority order (real Rails DSL never combines them on one push;
/// this order is just a deterministic tie-break if it somehow did).
fn extract_position(pairs: &[(String, &Node)]) -> Position {
    if kwarg(pairs, "first").and_then(bool_lit) == Some(true) {
        return Position::First;
    }
    if kwarg(pairs, "last").and_then(bool_lit) == Some(true) {
        return Position::Last;
    }
    if let Some(v) = kwarg(pairs, "before").and_then(sym_or_str) {
        return Position::Before(v);
    }
    if let Some(v) = kwarg(pairs, "after").and_then(sym_or_str) {
        return Position::After(v);
    }
    Position::Append
}

/// The each-loop expansion (module doc §"The dynamic per-key loop"): find
/// the single `<x>menu.push :"<prefix>#{…}", …` site inside the loop body
/// and replay it once per hash key, synthesizing `item = "<prefix><key>"`.
/// `parent`/position kwargs come from that ONE call's literal kwargs,
/// shared across every synthesized item (matches the real corpus: the
/// per-key variance lives in the `**options` double-splat, which never
/// carries position-relevant kwargs on this corpus).
fn expand_each_loop(menu: &str, keys: &[String], body: &Node, w: &mut Walker<'_>) {
    if let Some((prefix, args)) = find_push_with_dsym_prefix(body) {
        for key in keys {
            handle_push(menu, Some(format!("{prefix}{key}")), args, w);
        }
    }
}

/// Find a `<x>menu.push` call whose first positional arg is an
/// interpolated `Dsym` (`:"prefix#{expr}"`) — the each-loop item-symbol
/// shape. Returns the literal prefix + that call's full `args` slice (so
/// the caller can extract `parent:`/position kwargs from the very same
/// call). Transparent through `Begin`/`If`/`IfMod`.
fn find_push_with_dsym_prefix(node: &Node) -> Option<(String, &[Node])> {
    match node {
        Node::Begin(b) => b.statements.iter().find_map(find_push_with_dsym_prefix),
        Node::If(i) => i
            .if_true
            .as_deref()
            .and_then(find_push_with_dsym_prefix)
            .or_else(|| i.if_false.as_deref().and_then(find_push_with_dsym_prefix)),
        Node::IfMod(i) => i
            .if_true
            .as_deref()
            .and_then(find_push_with_dsym_prefix)
            .or_else(|| i.if_false.as_deref().and_then(find_push_with_dsym_prefix)),
        Node::Send(s) if is_menu_push_call(s) => {
            let positionals = positional_args(&s.args);
            match positionals.first().copied() {
                Some(Node::Dsym(d)) => dsym_prefix(d).map(|p| (p, s.args.as_slice())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The literal prefix of a `Dsym`'s parts (the `Str` segments,
/// concatenated) — `None` unless at least one part is a genuine
/// interpolation (a `Dsym` with only `Str` parts never occurs; lib-ruby-
/// parser's `symbol_compose` collapses that case to a plain `Sym` at parse
/// time — this check is defense-in-depth, not load-bearing).
fn dsym_prefix(d: &lib_ruby_parser::nodes::Dsym) -> Option<String> {
    let mut prefix = String::new();
    let mut has_interpolation = false;
    for part in &d.parts {
        match part {
            Node::Str(s) => prefix.push_str(&s.value.to_string_lossy()),
            _ => has_interpolation = true,
        }
    }
    has_interpolation.then_some(prefix)
}

// ─────────────────────────────────────────────────────────────────────────
// Arg-shape helpers (module-local copies of `routes.rs`'s idioms — small
// enough that sharing isn't worth a `pub(crate)` seam, matching this
// crate's existing per-module duplication convention)
// ─────────────────────────────────────────────────────────────────────────

fn sym_str(node: &Node) -> Option<String> {
    match node {
        Node::Sym(s) => Some(s.name.to_string_lossy()),
        _ => None,
    }
}

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

fn bool_lit(node: &Node) -> Option<bool> {
    match node {
        Node::True(_) => Some(true),
        Node::False(_) => Some(false),
        _ => None,
    }
}

/// Every `(key, value_node)` pair across all `Hash`/`Kwargs` args, in
/// declaration order. A `Kwsplat`/`BlockPass` entry (e.g. `**options`) is
/// silently skipped — only literal `key: value` pairs are load-bearing
/// for `parent`/`first`/`last`/`before`/`after`.
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

/// True positional args — everything in `args` that is NOT a `Hash`/
/// `Kwargs` node.
fn positional_args(args: &[Node]) -> Vec<&Node> {
    args.iter()
        .filter(|a| !matches!(a, Node::Hash(_) | Node::Kwargs(_)))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────
// §5 — the `if:`-guard permission extraction (OQ-GUARD-1)
//
// The visibility-honest harvest for [`Predicate::GuardedByPermission`]. A
// menu item's `if:` kwarg is a proc (`->(_) { ... }` / `Proc.new { ... }`)
// whose body is a boolean tree of `User.current.allowed_*?(:perm)` checks
// combined via `&&`/`||`, possibly alongside non-permission conditions
// (`Setting.*` / `admin?` / `logged?`). Both operands of a disjunction are
// GUARDED-BY the item's visibility, so BOTH are emitted — the honest
// weaker claim the OQ-GUARD-1 probe established (a flat "requires" would
// misencode the one real disjunction in the corpus). Dynamic permission
// arguments (a `Hash`/method-call, not a `Sym` literal) yield no symbol —
// nothing is fabricated.
// ─────────────────────────────────────────────────────────────────────────

/// Extract the deduped + sorted permission symbols from an `if:` kwarg
/// value. Unwraps the proc wrapper to its body, walks the boolean tree for
/// every `allowed_*` permission check, and keeps only the statically
/// resolvable `Sym`/`Str` arguments.
fn extract_guard_permissions(if_value: &Node) -> Vec<String> {
    let mut perms: Vec<String> = find_permission_symbols(guard_body(if_value));
    perms.sort();
    perms.dedup();
    perms
}

/// Unwrap the `if:` kwarg VALUE down to its callable body: `->(_) { ... }`
/// and `Proc.new { ... }` are both `Block { body, ... }`. Anything else (a
/// bare literal, or a `if: :method_name` symbol shorthand) has no
/// inspectable body and is treated as a leaf itself — a symbol shorthand is
/// genuinely unresolvable without cross-file analysis, so it yields no
/// permission rather than being miscounted as one.
fn guard_body(if_value: &Node) -> &Node {
    match if_value {
        Node::Block(blk) => blk.body.as_deref().unwrap_or(if_value),
        other => other,
    }
}

/// Walk the AND/OR spine of a guard body, collecting every `allowed_*`
/// permission SYMBOL (dynamic-arg calls contribute nothing). Structural
/// boolean nodes (`And`/`Or`/parenthesising `Begin`/`If`/`IfMod`) recurse;
/// any other node is a LEAF searched in full — so a permission check wrapped
/// in `.any?` / `!` / `&.` is still found.
fn find_permission_symbols(node: &Node) -> Vec<String> {
    let mut out = Vec::new();
    collect_permission_symbols(node, &mut out);
    out
}

fn collect_permission_symbols(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Send(s) => {
            if is_permission_method(&s.method_name)
                && let Some(sym) = permission_arg(&s.args)
            {
                out.push(sym);
            }
            if let Some(recv) = s.recv.as_deref() {
                collect_permission_symbols(recv, out);
            }
            for a in &s.args {
                collect_permission_symbols(a, out);
            }
        }
        Node::CSend(s) => {
            if is_permission_method(&s.method_name)
                && let Some(sym) = permission_arg(&s.args)
            {
                out.push(sym);
            }
            collect_permission_symbols(&s.recv, out);
            for a in &s.args {
                collect_permission_symbols(a, out);
            }
        }
        Node::And(a) => {
            collect_permission_symbols(&a.lhs, out);
            collect_permission_symbols(&a.rhs, out);
        }
        Node::Or(o) => {
            collect_permission_symbols(&o.lhs, out);
            collect_permission_symbols(&o.rhs, out);
        }
        Node::Begin(b) => {
            for stmt in &b.statements {
                collect_permission_symbols(stmt, out);
            }
        }
        Node::If(i) => {
            collect_permission_symbols(&i.cond, out);
            if let Some(t) = i.if_true.as_deref() {
                collect_permission_symbols(t, out);
            }
            if let Some(f) = i.if_false.as_deref() {
                collect_permission_symbols(f, out);
            }
        }
        Node::IfMod(i) => {
            collect_permission_symbols(&i.cond, out);
            if let Some(t) = i.if_true.as_deref() {
                collect_permission_symbols(t, out);
            }
            if let Some(f) = i.if_false.as_deref() {
                collect_permission_symbols(f, out);
            }
        }
        Node::Hash(h) => {
            for p in &h.pairs {
                collect_permission_symbols(p, out);
            }
        }
        Node::Pair(p) => {
            collect_permission_symbols(&p.key, out);
            collect_permission_symbols(&p.value, out);
        }
        Node::Array(arr) => {
            for e in &arr.elements {
                collect_permission_symbols(e, out);
            }
        }
        Node::Kwargs(k) => {
            for p in &k.pairs {
                collect_permission_symbols(p, out);
            }
        }
        Node::Block(blk) => {
            collect_permission_symbols(&blk.call, out);
            if let Some(body) = blk.body.as_deref() {
                collect_permission_symbols(body, out);
            }
        }
        _ => {}
    }
}

/// A permission check is any `allowed_*` method (with or without a trailing
/// `?` — `allowed_to`, `allowed_globally?`, `allowed_in_project?`,
/// `allowed_in_any_work_package?`, ...).
fn is_permission_method(method_name: &str) -> bool {
    method_name.starts_with("allowed_")
}

/// The permission symbol from an `allowed_*` call's arguments. Normal form
/// (`allowed_globally?(:sym)`) puts it FIRST; the receiver-style
/// `allowed_to(User.current, :sym)` / `Project.portfolio.allowed_to(
/// User.current, :view_project)` shape puts a user-expression first and the
/// permission SECOND. Returns `None` when neither slot is a `Sym`/`Str`
/// literal (a dynamic `Hash`/method-call argument — never fabricated).
fn permission_arg(args: &[Node]) -> Option<String> {
    if let Some(sym) = args.first().and_then(sym_or_str) {
        return Some(sym);
    }
    args.get(1).and_then(sym_or_str)
}

impl RegionEntry {
    /// Project this Rails harvest record onto the shared, frontend-agnostic
    /// [`RegionFact`]. The Rails-local subject convention (`{ns}:{menu}` as the
    /// screen, the bare `menu` token as the `docked_at` object) is applied
    /// here; the emission itself lives once in [`RegionFact::to_triples`], so
    /// the subject grammar is shared with the Odoo and `WinForms` arms and the
    /// [`ruff_spo_triplet::build_nav_digest`] consumer.
    #[must_use]
    pub fn to_fact(&self, namespace: &str) -> RegionFact {
        RegionFact {
            screen: format!("{namespace}:{}", self.menu),
            control: self.item.clone(),
            dock_token: self.menu.clone(),
            tab_order: self.tab_order,
            opens_popup: None,
            parent: self.parent.clone(),
        }
    }

    /// Lift into the shared closed-vocab triples via [`Self::to_fact`]:
    /// `docked_at` always, `tab_order` when resolved, and `contains_control`
    /// when `parent` is present. Byte-identical to the pre-collapse hand-rolled
    /// emission (subject `{ns}:{menu}.{item}`, `docked_at → menu`).
    #[must_use]
    pub fn to_triples(&self, namespace: &str) -> Vec<Triple> {
        self.to_fact(namespace).to_triples()
    }

    /// Project this menu item onto the shared, frontend-agnostic [`MenuQuad`]
    /// (the `location` + `purpose` axes of the Klickwege menu quad). The node
    /// and parent use the BARE `{ns}:{name}` grammar — identical to the nav
    /// arm's `navigates_to` subjects, NOT the region plane's `{menu}.{item}`
    /// composite (the quad is the menu-tree/navigation plane, not the
    /// within-screen layout plane). `part_of` is Authoritative (Rails declares
    /// the parent via `parent:`); `purpose` classifies the target `action:`
    /// (a bare `controller:` defaults to Rails' `index`) through the shared
    /// [`classify_purpose`] engine + the `RAILS_PURPOSE` config.
    #[must_use]
    pub fn to_quad(&self, namespace: &str) -> MenuQuad {
        let token = match &self.action {
            Some(a) => a.as_str(),
            // A push with a `controller:` but no `action:` is the REST-style
            // `index` default; a push with no target at all has no signal.
            None if self.has_controller => "index",
            None => "",
        };
        let purpose = classify_purpose(&[token], RAILS_PURPOSE, RAILS_PURPOSE_FALLBACK);
        MenuQuad {
            node: format!("{namespace}:{}", self.item),
            parent: self.parent.as_ref().map(|p| format!("{namespace}:{p}")),
            part_of_tier: Provenance::Authoritative,
            purpose,
            // Identity is dormant here by design — [`extract_menu_quads`]
            // binds it in a post-pass (roster cross-check needs `root`,
            // which this per-entry projection doesn't have).
            identity_concept: None,
            identity_tier: Provenance::Authoritative,
        }
    }

    /// Emit one [`Predicate::GuardedByPermission`] fact per extracted `if:`-guard
    /// permission symbol. The subject is the BARE `{namespace}:{item}` node —
    /// the same grammar as [`Self::to_quad`]'s node and the nav arm's
    /// `navigates_to` subject, NOT the region plane's `{menu}.{item}` composite
    /// (the guard is a property of the menu-tree NODE, not of a within-screen
    /// control placement). The object is the permission symbol; the tier is
    /// [`Provenance::Inferred`] (a proc-body heuristic). Empty when no symbol
    /// was extracted.
    #[must_use]
    pub fn to_guard_triples(&self, namespace: &str) -> Vec<Triple> {
        let node = format!("{namespace}:{}", self.item);
        self.permissions
            .iter()
            .map(|perm| {
                Triple::new(
                    node.clone(),
                    Predicate::GuardedByPermission,
                    perm.clone(),
                    Provenance::Inferred,
                )
            })
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The `surfaces_concept` identity-binding arm (SPEC v2, council-consolidated)
//
// `controller -> model` is DERIVED, not declared config (unlike Odoo's
// `res_model` or C#'s `roomAliases`) — deterministic (the same singularize
// inflection the schema arm uses) but not curated, so it is emitted ONLY
// after a cross-check against the REAL model roster (the set of models
// actually backed by a DB table, per `crate::schema::model_roster`). A
// derived token that doesn't match the roster is a visible refusal
// (`derived_unmatched`), never a fabricated `surfaces_concept`.
// ─────────────────────────────────────────────────────────────────────────

/// One entry's identity-binding outcome — the 5-bucket conservation ledger's
/// per-entry classification, index-aligned with the entry slice (same
/// discipline as [`resolve_tab_orders`]'s parallel `Option<u32>` vec).
#[derive(Debug, Clone, PartialEq, Eq)]
enum IdentityBinding {
    /// No `controller:` target at all (`has_controller == false`) —
    /// identity dormant BY DESIGN (a settings/help/external-URL item has no
    /// backing model to bind).
    WithoutConcept,
    /// A `controller:` kwarg IS present (`has_controller == true`) but its
    /// value is DYNAMIC (`controller: options[:controller]`) — no static
    /// token to derive from, so nothing is emitted, but it is NOT
    /// "concept-less by design": there IS a target we can't yet resolve.
    /// Counted separately so a dynamic target is never hidden inside
    /// [`Self::WithoutConcept`].
    DynamicController,
    /// A `controller:`-derived model token that is NOT in the real model
    /// roster — the visible failure-rate bucket (irregular plurals, a
    /// namespaced controller the `/`-fix doesn't cover, or a genuinely
    /// model-less controller). NOT emitted.
    DerivedUnmatched,
    /// A `controller:`-derived model token found in the roster — the
    /// honest [`Provenance::OpenProjectExtracted`] emission.
    DerivedMatched(String),
}

/// `controller:` value -> candidate model-name token: strip a leading `/`,
/// take the LAST `/`-segment (so `admin/settings` inflects on its meaningful
/// stem `settings`, NOT the garbage `Admin/setting` a naive `_`-split would
/// produce), then run it through the SAME table->model inflection the
/// schema arm uses ([`crate::schema::model_name_for_table`]) — no
/// duplicated `IRREGULAR` table (E4 discipline, `routes.rs`'s
/// `singularize_local` comment).
fn derive_model_from_controller(controller: &str) -> String {
    let stem = controller.trim_start_matches('/');
    let last = stem.rsplit('/').next().unwrap_or(stem);
    crate::schema::model_name_for_table(last)
}

/// Cross-check every entry's `controller:` target against the real model
/// `roster`, deriving via [`derive_model_from_controller`]. One
/// [`IdentityBinding`] per entry, index-aligned with `entries`. The
/// `has_controller`/`controller` pair distinguishes THREE controller states:
/// absent (`WithoutConcept`), present-but-dynamic (`DynamicController`), and
/// present-and-static (derive + roster check).
fn bind_identities(entries: &[RegionEntry], roster: &HashSet<String>) -> Vec<IdentityBinding> {
    entries
        .iter()
        .map(|e| match &e.controller {
            // A dynamic `controller:` records `has_controller == true` but no
            // static value — a target we can't resolve, NOT a design-dormant
            // absence.
            None if e.has_controller => IdentityBinding::DynamicController,
            None => IdentityBinding::WithoutConcept,
            Some(controller) => {
                let derived = derive_model_from_controller(controller);
                if roster.contains(&derived) {
                    IdentityBinding::DerivedMatched(derived)
                } else {
                    IdentityBinding::DerivedUnmatched
                }
            }
        })
        .collect()
}

/// Harvest every menu item as a [`MenuQuad`] — the `location`/`purpose` half of
/// the Klickwege menu quad, PLUS the `identity` axis (`surfaces_concept`):
/// each entry's `controller:` target is cross-checked against the real model
/// roster (`crate::schema::model_roster`) via `bind_identities`; a
/// roster match binds `identity_concept` at
/// [`Provenance::OpenProjectExtracted`], everything else stays dormant
/// (`None`, [`RegionEntry::to_quad`]'s default). Companion to
/// [`extract_regions`] (the layout plane); both read the same `menu.push`
/// sites. Nodes use the bare `{namespace}:{item}` grammar.
#[must_use]
pub fn extract_menu_quads(root: &Path, namespace: &str) -> Vec<MenuQuad> {
    let entries = extract_regions(root);
    let roster = crate::schema::model_roster(root);
    let bindings = bind_identities(&entries, &roster);
    entries
        .iter()
        .zip(bindings)
        .map(|(e, binding)| {
            let mut quad = e.to_quad(namespace);
            if let IdentityBinding::DerivedMatched(model) = binding {
                quad.identity_concept = Some(model);
                quad.identity_tier = Provenance::OpenProjectExtracted;
            }
            quad
        })
        .collect()
}

/// Harvest every menu item's `if:`-guard permissions as
/// [`Predicate::GuardedByPermission`] triples (the OQ-GUARD-1 visibility rail).
/// Companion to [`extract_menu_quads`]; both read the same `menu.push` sites.
/// Nodes use the bare `{namespace}:{item}` grammar. Items whose guard names no
/// statically-extractable permission contribute nothing.
#[must_use]
pub fn extract_menu_guards(root: &Path, namespace: &str) -> Vec<Triple> {
    extract_regions(root)
        .iter()
        .flat_map(|e| e.to_guard_triples(namespace))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    use ruff_spo_triplet::{ExamConfig, build_nav_digest};

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn scratch_dir(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_region_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// Fixture (a) — single map block, plain appends → declaration-order
    /// `tab_order`.
    #[test]
    fn plain_appends_get_declaration_order_tab_order() {
        let root = scratch_dir("plain_appends");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }\n\
             \x20 menu.push :beta, { controller: \"/b\" }\n\
             \x20 menu.push :gamma, { controller: \"/c\" }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        assert_eq!(entries.len(), 3, "{entries:?}");
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("alpha"), Some(0));
        assert_eq!(order("beta"), Some(1));
        assert_eq!(order("gamma"), Some(2));
        for e in &entries {
            assert_eq!(e.menu, "top_menu");
            assert!(e.parent.is_none());
        }

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (b) — `parent:` nesting → `contains_control` + a SEPARATE
    /// per-parent sibling ordering (root items and `:settings`'s children
    /// each form their own group).
    #[test]
    fn parent_nesting_emits_contains_control_and_own_sibling_order() {
        let root = scratch_dir("parent_nesting");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :project_menu do |menu|\n\
             \x20 menu.push :settings, { controller: \"/settings\" }\n\
             \x20 menu.push :settings_general, { controller: \"/g\" }, parent: :settings\n\
             \x20 menu.push :settings_modules, { controller: \"/m\" }, parent: :settings\n\
             \x20 menu.push :activity, { controller: \"/act\" }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let by_item = |name: &str| entries.iter().find(|e| e.item == name).unwrap();

        assert_eq!(by_item("settings").tab_order, Some(0));
        assert_eq!(by_item("activity").tab_order, Some(1));
        assert!(by_item("settings").parent.is_none());

        assert_eq!(
            by_item("settings_general").parent.as_deref(),
            Some("settings")
        );
        assert_eq!(by_item("settings_general").tab_order, Some(0));
        assert_eq!(by_item("settings_modules").tab_order, Some(1));

        let triples = by_item("settings_general").to_triples("openproject");
        assert!(triples.iter().any(|t| {
            t.s == "openproject:project_menu.settings"
                && t.p == "contains_control"
                && t.o == "openproject:project_menu.settings_general"
        }));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (c) — `first:`/`last:` reordering.
    #[test]
    fn first_and_last_reorder_within_sibling_group() {
        let root = scratch_dir("first_last");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }\n\
             \x20 menu.push :beta, { controller: \"/b\" }, last: true\n\
             \x20 menu.push :gamma, { controller: \"/c\" }, first: true\n\
             \x20 menu.push :delta, { controller: \"/d\" }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        // gamma (first) -> 0; alpha, delta keep relative decl order -> 1, 2;
        // beta (last) -> 3.
        assert_eq!(order("gamma"), Some(0));
        assert_eq!(order("alpha"), Some(1));
        assert_eq!(order("delta"), Some(2));
        assert_eq!(order("beta"), Some(3));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (d) — `before:`/`after:` with an in-group anchor, including
    /// a chain resolved in declaration order.
    #[test]
    fn before_and_after_reposition_relative_to_anchor() {
        let root = scratch_dir("before_after");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }\n\
             \x20 menu.push :beta, { controller: \"/b\" }\n\
             \x20 menu.push :gamma, { controller: \"/c\" }, before: :alpha\n\
             \x20 menu.push :delta, { controller: \"/d\" }, after: :alpha\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        // decl: alpha, beta, gamma, delta.
        // gamma before:alpha -> [gamma, alpha, beta]
        // delta after:alpha (alpha's CURRENT position) -> [gamma, alpha, delta, beta]
        assert_eq!(order("gamma"), Some(0));
        assert_eq!(order("alpha"), Some(1));
        assert_eq!(order("delta"), Some(2));
        assert_eq!(order("beta"), Some(3));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (e) — `after:` with a MISSING anchor → Rails' documented
    /// missing-anchor behavior: `exists?(:nonexistent)` is false at push
    /// time, so the `after` branch is skipped and beta falls through to a
    /// plain `add` AT THAT MOMENT (index 1, before the not-yet-pushed
    /// gamma) — NOT an append to the very end. (The earlier phase-separated
    /// model wrongly pushed beta past gamma; the single-pass replay keeps
    /// Rails' declaration-time placement.)
    #[test]
    fn after_missing_anchor_falls_back_to_plain_add_at_push_time() {
        let root = scratch_dir("missing_anchor");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }\n\
             \x20 menu.push :beta, { controller: \"/b\" }, after: :nonexistent\n\
             \x20 menu.push :gamma, { controller: \"/c\" }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("alpha"), Some(0));
        assert_eq!(order("beta"), Some(1));
        assert_eq!(order("gamma"), Some(2));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (f) — cross-file: two files pushing to the same menu →
    /// merged order. Exercises `walk_top`'s generic descent through
    /// `Module` → `Class` → `Block` (`register`) → `Block` (`.map`), the
    /// real module-engine shape (`github_integration`/`backlogs`/… all
    /// wrap their `MenuManager.map` this way).
    #[test]
    fn cross_file_pushes_to_same_menu_merge_into_one_order() {
        let root = scratch_dir("cross_file");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :admin_menu do |menu|\n\
             \x20 menu.push :core_item, { controller: \"/core\" }\n\
             end\n",
        );
        write_file(
            &root,
            "modules/plugin_a/lib/plugin_a/engine.rb",
            "module PluginA\n\
             \x20 class Engine < ::Rails::Engine\n\
             \x20   register(\"plugin_a\") do\n\
             \x20     ::Redmine::MenuManager.map(:admin_menu) do |menu|\n\
             \x20       menu.push :plugin_item, { controller: \"/plugin\" }, after: :core_item\n\
             \x20     end\n\
             \x20   end\n\
             \x20 end\n\
             end\n",
        );

        let entries = extract_regions(&root);
        assert_eq!(entries.len(), 2, "{entries:?}");
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("core_item"), Some(0));
        assert_eq!(order("plugin_item"), Some(1));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (g) — multi-line push (item + url + `if:` + `caption:`
    /// across lines) parses (the AST-walk, not a line-scanner, guarantee).
    #[test]
    fn multiline_push_parses() {
        let root = scratch_dir("multiline");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :portfolios,\n\
             \x20           { controller: \"/portfolios\", action: \"index\" },\n\
             \x20           context: :modules,\n\
             \x20           caption: \"Portfolios\",\n\
             \x20           if: ->(_) { true }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].item, "portfolios");
        assert_eq!(entries[0].menu, "top_menu");
        assert_eq!(entries[0].tab_order, Some(0));

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (h) — `docked_at` menu-name token + `to_triples` shapes.
    #[test]
    fn to_triples_emits_docked_at_and_tab_order() {
        let entry = RegionEntry {
            menu: "top_menu".to_string(),
            item: "projects".to_string(),
            parent: None,
            position: Position::Append,
            tab_order: Some(3),
            action: None,
            has_controller: false,
            controller: None,
            permissions: vec![],
            file: "config/initializers/menus.rb".to_string(),
        };
        let triples = entry.to_triples("openproject");
        assert!(triples.iter().any(|t| {
            t.s == "openproject:top_menu.projects" && t.p == "docked_at" && t.o == "top_menu"
        }));
        assert!(
            triples
                .iter()
                .any(|t| t.s == "openproject:top_menu.projects"
                    && t.p == "tab_order"
                    && t.o == "3")
        );
        // No parent -> no contains_control triple.
        assert!(!triples.iter().any(|t| t.p == "contains_control"));
        assert_eq!(triples.len(), 2, "{triples:?}");
    }

    /// Menu-quad emission: the location (`part_of` from `parent:`, bare
    /// `{ns}:{item}` grammar) + purpose (from the target `action:` through the
    /// shared engine) axes. `index`→list, `show`→detail, `new`/`edit`→form,
    /// a bare `controller:` defaults to `index`→list, a custom action→action.
    #[test]
    fn menu_items_project_onto_the_shared_quad() {
        let root = scratch_dir("menu_quad");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :project_menu do |menu|\n\
             \x20 menu.push :overview, { controller: \"/projects\", action: \"show\" }\n\
             \x20 menu.push :work_packages, { controller: \"/work_packages\", action: \"index\" }, parent: :overview\n\
             \x20 menu.push :new_wp, { controller: \"/work_packages\", action: \"new\" }, parent: :work_packages\n\
             \x20 menu.push :settings, { controller: \"/projects/settings\" }, parent: :overview\n\
             \x20 menu.push :board, { controller: \"/boards\", action: \"kanban\" }, parent: :overview\n\
             end\n",
        );

        let quads = extract_menu_quads(&root, "openproject");
        let q = |item: &str| {
            quads
                .iter()
                .find(|q| q.node == format!("openproject:{item}"))
                .unwrap_or_else(|| panic!("no quad for {item}"))
        };
        // purpose from action:
        assert_eq!(q("overview").purpose, PurposeRole::Detail); // show
        assert_eq!(q("work_packages").purpose, PurposeRole::List); // index
        assert_eq!(q("new_wp").purpose, PurposeRole::Form); // new
        assert_eq!(q("settings").purpose, PurposeRole::List); // controller, no action -> index
        assert_eq!(q("board").purpose, PurposeRole::Action); // custom "kanban" -> fallback
        // location: bare-node part_of rail (walk yields the radix address).
        assert_eq!(
            q("work_packages").parent.as_deref(),
            Some("openproject:overview")
        );
        assert_eq!(
            q("new_wp").parent.as_deref(),
            Some("openproject:work_packages")
        );
        assert_eq!(q("overview").parent, None); // root of project_menu
        // the emitted facts carry the bare grammar + Authoritative part_of.
        let wp = q("work_packages").to_triples();
        let part_of = wp.iter().find(|t| t.p == "part_of").unwrap();
        assert_eq!(part_of.s, "openproject:work_packages");
        assert_eq!(part_of.o, "openproject:overview");

        let _ = fs::remove_dir_all(&root);
    }

    // ─────────────────────────────────────────────────────────────────────
    // The `surfaces_concept` identity-binding arm (SPEC v2)
    // ─────────────────────────────────────────────────────────────────────

    /// Write a minimal `db/migrate/tables/<table>.rb` baseline-squash
    /// fixture so `crate::schema::model_roster` sees `<table>` as a
    /// real, table-backed model — the roster the identity-binding arm
    /// cross-checks derived tokens against.
    fn write_roster_table(root: &Path, table: &str) {
        write_file(
            root,
            &format!("db/migrate/tables/{table}.rb"),
            &format!("create_table :{table} do |t|\n  t.string :name\nend\n"),
        );
    }

    /// A `controller:`-derived token that MATCHES the roster (`work_packages`
    /// -> `WorkPackage`, present in the fixture roster) binds
    /// `identity_concept` at `OpenProjectExtracted`, and the emitted
    /// `surfaces_concept` triple carries that tier's `(0.95, 0.88)` truth —
    /// NOT the hardcoded `Authoritative` the pre-fix `to_triples` emitted.
    #[test]
    fn derived_identity_matched_binds_open_project_extracted() {
        let root = scratch_dir("identity_matched");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :work_packages, { controller: \"/work_packages\", action: \"index\" }\n\
             end\n",
        );
        write_roster_table(&root, "work_packages");

        let (entries, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.with_concept_derived_matched, 1, "{report:?}");
        assert_eq!(report.with_concept_derived_unmatched, 0, "{report:?}");
        assert_eq!(entries.len(), 1, "{entries:?}");

        let quads = extract_menu_quads(&root, "openproject");
        let q = quads
            .iter()
            .find(|q| q.node == "openproject:work_packages")
            .unwrap();
        assert_eq!(q.identity_concept.as_deref(), Some("WorkPackage"));
        assert_eq!(q.identity_tier, Provenance::OpenProjectExtracted);

        let triples = q.to_triples();
        let sc = triples
            .iter()
            .find(|t| t.p == "surfaces_concept")
            .unwrap_or_else(|| panic!("no surfaces_concept triple: {triples:?}"));
        assert_eq!(sc.o, "WorkPackage");
        assert_eq!(
            (sc.f, sc.c),
            Provenance::OpenProjectExtracted.truth(),
            "must NOT be the hardcoded Authoritative tier"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A namespaced controller (`admin/settings`) derives on its LAST
    /// `/`-segment (`settings` -> `Setting`), not a naive `_`-split garbage
    /// token (`Admin/setting`) — the `/`-aware fix (SPEC v2 §5).
    #[test]
    fn namespaced_controller_derives_on_last_path_segment() {
        let root = scratch_dir("identity_namespaced");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :admin_menu do |menu|\n\
             \x20 menu.push :settings, { controller: \"/admin/settings\" }\n\
             end\n",
        );
        write_roster_table(&root, "settings");

        let quads = extract_menu_quads(&root, "openproject");
        let q = quads
            .iter()
            .find(|q| q.node == "openproject:settings")
            .unwrap();
        assert_eq!(q.identity_concept.as_deref(), Some("Setting"));
        assert_eq!(q.identity_tier, Provenance::OpenProjectExtracted);

        let (_, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.with_concept_derived_matched, 1, "{report:?}");

        let _ = fs::remove_dir_all(&root);
    }

    /// A `controller:`-derived token that does NOT match the roster (no
    /// backing table for `widgets` in this fixture) emits NO
    /// `surfaces_concept` — a visible refusal counted in
    /// `derived_unmatched`, never a fabricated binding.
    #[test]
    fn derived_identity_unmatched_emits_nothing() {
        let root = scratch_dir("identity_unmatched");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :widgets, { controller: \"/widgets\", action: \"index\" }\n\
             end\n",
        );
        // No roster fixture at all -> no table backs `widgets`.

        let (_, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.with_concept_derived_matched, 0, "{report:?}");
        assert_eq!(report.with_concept_derived_unmatched, 1, "{report:?}");

        let quads = extract_menu_quads(&root, "openproject");
        let q = quads
            .iter()
            .find(|q| q.node == "openproject:widgets")
            .unwrap();
        assert_eq!(q.identity_concept, None);
        assert!(
            !q.to_triples().iter().any(|t| t.p == "surfaces_concept"),
            "unmatched derivation must not emit surfaces_concept"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A menu item with no `controller:` target at all (a URL/settings-only
    /// push) is identity-dormant BY DESIGN — `without_concept`, not a
    /// failure to derive anything.
    #[test]
    fn no_controller_target_is_without_concept() {
        let root = scratch_dir("identity_without_concept");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :help, \"https://example.com/help\"\n\
             end\n",
        );

        let (_, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.without_concept, 1, "{report:?}");
        assert_eq!(report.with_dynamic_controller, 0, "{report:?}");
        assert_eq!(report.with_concept_derived_matched, 0, "{report:?}");
        assert_eq!(report.with_concept_derived_unmatched, 0, "{report:?}");

        let quads = extract_menu_quads(&root, "openproject");
        let q = quads.iter().find(|q| q.node == "openproject:help").unwrap();
        assert_eq!(q.identity_concept, None);

        let _ = fs::remove_dir_all(&root);
    }

    /// A menu item whose `controller:` value is DYNAMIC (the each-loop
    /// `controller: options[:controller]` case) records `has_controller ==
    /// true` but no static token — it is counted in `with_dynamic_controller`,
    /// NOT `without_concept` (which means "no target at all"). Nothing is
    /// emitted either way, but the ledger no longer hides a dynamic target as
    /// intentionally concept-less (PR #87 codex P2).
    #[test]
    fn dynamic_controller_is_not_without_concept() {
        let root = scratch_dir("identity_dynamic_controller");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :project_menu do |menu|\n\
             \x20 menu.push :dynamic_item, { controller: options[:controller], action: \"index\" }\n\
             end\n",
        );

        let (_, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.with_dynamic_controller, 1, "{report:?}");
        assert_eq!(report.without_concept, 0, "{report:?}");
        assert_eq!(report.with_concept_derived_matched, 0, "{report:?}");
        assert_eq!(report.with_concept_derived_unmatched, 0, "{report:?}");

        // A dynamic controller emits no surfaces_concept (no static token).
        let quads = extract_menu_quads(&root, "openproject");
        let q = quads
            .iter()
            .find(|q| q.node == "openproject:dynamic_item")
            .unwrap();
        assert_eq!(q.identity_concept, None);
        assert!(
            !q.to_triples().iter().any(|t| t.p == "surfaces_concept"),
            "dynamic controller must not emit surfaces_concept"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Fixture (i) — a mutual `after:` reference ("cycle") resolves
    /// DETERMINISTICALLY under the single-pass replay, and is therefore NOT
    /// unresolved. Rails processes registrations in declaration order and
    /// checks `exists?` at push time, so a forward reference to a
    /// not-yet-pushed anchor falls through to a plain `add`; there is no
    /// unresolvable state to detect. (The earlier phase-separated model
    /// treated this as a cycle and emitted `unresolved_order`; that was a
    /// divergence from Rails, caught by the correctness adversary.)
    ///   alpha after:beta -> beta absent -> plain add -> [alpha]
    ///   beta  after:alpha -> alpha at 0 -> `add_at` 1  -> [alpha, beta]
    #[test]
    fn mutual_after_reference_resolves_deterministically_single_pass() {
        let root = scratch_dir("mutual_after");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }, after: :beta\n\
             \x20 menu.push :beta, { controller: \"/b\" }, after: :alpha\n\
             end\n",
        );

        let (entries, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.unresolved_order, 0, "{report:?}");
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("alpha"), Some(0));
        assert_eq!(order("beta"), Some(1));

        let _ = fs::remove_dir_all(&root);
    }

    /// Finding 1 (correctness adversary) — multiple `first:` items are
    /// **LIFO**, not FIFO. Rails `prepend` inserts each `first:` item at
    /// index 0, so the LAST-declared `first:` wins the front. A phase-
    /// separated "collect all firsts, move to front in declaration order"
    /// model would give FIFO (alpha=0) — the confirmed bug.
    ///   alpha first -> [alpha]
    ///   beta  plain -> [alpha, beta]
    ///   gamma first -> prepend -> [gamma, alpha, beta]
    #[test]
    fn multiple_first_items_are_lifo_not_fifo() {
        let root = scratch_dir("first_lifo");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }, first: true\n\
             \x20 menu.push :beta, { controller: \"/b\" }\n\
             \x20 menu.push :gamma, { controller: \"/c\" }, first: true\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("gamma"), Some(0));
        assert_eq!(order("alpha"), Some(1));
        assert_eq!(order("beta"), Some(2));

        let _ = fs::remove_dir_all(&root);
    }

    /// Finding 2 (correctness adversary) — a plain push after a `before:`/
    /// `after:` splice onto a `last:` item lands relative to the **live**
    /// `size - last_count` boundary. `after:` uses `add_at` (does NOT bump
    /// `last_count`), so the trailing `last:` band is still {a}; the plain
    /// `c` inserts just before it. A phase-separated model that applied Last
    /// after Before/After would push `b` to the very end — the confirmed bug.
    ///   a last  -> push, `last_count=1`        -> [a]
    ///   b after:a -> a at 0 -> `add_at` 1      -> [a, b]  (`last_count` still 1)
    ///   c plain -> insert at 2-1=1           -> [a, c, b]
    #[test]
    fn plain_push_after_splice_onto_last_respects_live_boundary() {
        let root = scratch_dir("last_splice");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :a, { controller: \"/a\" }, last: true\n\
             \x20 menu.push :b, { controller: \"/b\" }, after: :a\n\
             \x20 menu.push :c, { controller: \"/c\" }\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let order = |name: &str| entries.iter().find(|e| e.item == name).unwrap().tab_order;
        assert_eq!(order("a"), Some(0));
        assert_eq!(order("c"), Some(1));
        assert_eq!(order("b"), Some(2));

        let _ = fs::remove_dir_all(&root);
    }

    /// The each-loop expansion (module doc §"The dynamic per-key loop") —
    /// the real `menus.rb:808` `:settings` shape: a literal hash-of-syms
    /// local var, iterated with `.each do |key, options|`, pushed via an
    /// interpolated `:"settings_#{key}"` symbol with a literal `parent:`.
    #[test]
    fn hash_var_each_loop_expands_dynamic_items_with_parent() {
        let root = scratch_dir("each_loop");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :project_menu do |menu|\n\
             \x20 menu.push :settings, { controller: \"/settings\" }\n\
             \x20 project_menu_items = {\n\
             \x20   general: { caption: :label_general },\n\
             \x20   modules: { caption: :label_modules }\n\
             \x20 }\n\
             \x20 project_menu_items.each do |key, options|\n\
             \x20   menu.push :\"settings_#{key}\",\n\
             \x20             { controller: options[:controller] },\n\
             \x20             parent: :settings,\n\
             \x20             **options\n\
             \x20 end\n\
             end\n",
        );

        let entries = extract_regions(&root);
        let names: Vec<&str> = entries.iter().map(|e| e.item.as_str()).collect();
        assert!(names.contains(&"settings_general"), "{names:?}");
        assert!(names.contains(&"settings_modules"), "{names:?}");
        let general = entries
            .iter()
            .find(|e| e.item == "settings_general")
            .unwrap();
        assert_eq!(general.parent.as_deref(), Some("settings"));
        assert_eq!(general.menu, "project_menu");

        let _ = fs::remove_dir_all(&root);
    }

    /// Corpus probe (§8, the [H]->[G] gate) — env-gated, self-skipping.
    /// Runs over the real `OpenProject` corpus and pins the measured
    /// invariants: `map_blocks` (>= the 9 core menus; module engines add
    /// more), `items` (>= the ~117 measured raw `menu.push` sites),
    /// `with_parent > 0`, and `unresolved_order == 0` (no genuine
    /// before/after cycles on this corpus). Plus the two spot-checks: the
    /// `:settings` under `:project_menu` dynamic-loop children resolve
    /// with `contains_control`, and `:top_menu` items resolve.
    #[test]
    #[allow(clippy::print_stderr)] // diagnostic emission gated on env var (real-corpus gate)
    fn corpus_probe_region_arm_over_openproject() {
        let Ok(root) = std::env::var("RAILS_CORPUS_SRC") else {
            eprintln!("RAILS_CORPUS_SRC unset; skipping region-arm corpus probe");
            return;
        };
        let path = Path::new(&root);
        let (entries, report) = extract_regions_with_report(path, "openproject");

        assert!(report.map_blocks >= 9, "{report:?}");
        assert!(report.items >= 117, "{report:?}");
        assert!(report.with_parent > 0, "{report:?}");
        assert_eq!(report.unresolved_order, 0, "{report:?}");

        // :settings under :project_menu resolves with contains_control to
        // its settings_* children (menus.rb:809 dynamic each-loop).
        let settings_children: Vec<&RegionEntry> = entries
            .iter()
            .filter(|e| e.menu == "project_menu" && e.parent.as_deref() == Some("settings"))
            .collect();
        assert!(!settings_children.is_empty(), "{report:?}");
        assert!(
            settings_children
                .iter()
                .any(|e| e.item == "settings_general"),
            "{settings_children:?}"
        );

        // :top_menu items resolve (the region-name -> top_bar mapping
        // itself is downstream config, out of scope for this harvester).
        let top_menu_items: Vec<&RegionEntry> =
            entries.iter().filter(|e| e.menu == "top_menu").collect();
        assert!(!top_menu_items.is_empty(), "{report:?}");

        // The `surfaces_concept` identity-binding 5-bucket ledger (SPEC v2):
        // the CRUD spine (`work_packages`/`projects`/`time_entries`) is the
        // load-bearing nav core, so at least one derived token must resolve
        // against the real model roster.
        assert!(report.with_concept_derived_matched >= 1, "{report:?}");
        // Conservation: every entry lands in EXACTLY one bucket, so the five
        // buckets sum to the entry count (each `menu.push` is one entry).
        assert_eq!(
            report.without_concept
                + report.with_dynamic_controller
                + report.with_concept_declared
                + report.with_concept_derived_matched
                + report.with_concept_derived_unmatched,
            entries.len(),
            "{report:?}"
        );

        eprintln!(
            "region-arm corpus probe: {} files, {} map_blocks, {} items, {} with_parent, {} unresolved",
            report.files_scanned,
            report.map_blocks,
            report.items,
            report.with_parent,
            report.unresolved_order,
        );
        eprintln!(
            "identity-binding ledger: without_concept={} with_dynamic_controller={} \
             with_concept_declared={} with_concept_derived_matched={} \
             with_concept_derived_unmatched={}",
            report.without_concept,
            report.with_dynamic_controller,
            report.with_concept_declared,
            report.with_concept_derived_matched,
            report.with_concept_derived_unmatched,
        );
    }

    /// Non-corpus fixture — the twin of `ruff_spo_triplet::nav_digest::tests::
    /// menu_quad_lowers_location_as_a_classid_radix_path`, but for the BARE
    /// fallback path this hand-built triple set exercises: no `part_of`
    /// ancestor here binds `identity_concept` (`None` on every quad below,
    /// mirroring `to_quad`'s own dormant default — the roster-verified
    /// binding lives in `extract_menu_quads`'s post-pass, not in `to_quad`
    /// itself), so no `surfaces_concept`/classid triple exists for
    /// `menu_address` to resolve and its fallback (bare screen name per
    /// ancestor) is the only path THIS fixture can hit. Hand-build a
    /// 3-level chain (root -> child -> grandchild) via `MenuQuad::parent`,
    /// lower through `build_nav_digest`, and assert the grandchild's `loc`
    /// is the root-first 3-segment bare-name path with `action=navigate`,
    /// while the parentless root gets `action=leaf` (NOT `root` — the
    /// `root` action requires a `navigates_to`/`selects_view` out-edge,
    /// which a menu-quad-only triple set never carries; see the corpus
    /// probe below for the same finding on real data).
    #[test]
    fn menu_quad_round_trip_lowers_bare_name_chain_without_classid() {
        let quads = [
            MenuQuad {
                node: "app:root_item".to_string(),
                parent: None,
                part_of_tier: Provenance::Authoritative,
                purpose: PurposeRole::List,
                identity_concept: None,
                identity_tier: Provenance::Authoritative,
            },
            MenuQuad {
                node: "app:child_item".to_string(),
                parent: Some("app:root_item".to_string()),
                part_of_tier: Provenance::Authoritative,
                purpose: PurposeRole::Detail,
                identity_concept: None,
                identity_tier: Provenance::Authoritative,
            },
            MenuQuad {
                node: "app:grandchild_item".to_string(),
                parent: Some("app:child_item".to_string()),
                part_of_tier: Provenance::Authoritative,
                purpose: PurposeRole::Form,
                identity_concept: None,
                identity_tier: Provenance::Authoritative,
            },
        ];
        let triples: Vec<Triple> = quads.iter().flat_map(MenuQuad::to_triples).collect();
        let digest = build_nav_digest(&triples, &ExamConfig::default());
        assert!(
            digest.contains(
                "grandchild_item  loc=root_item/child_item/grandchild_item  purpose=form  id=-  action=navigate"
            ),
            "grandchild must lower to the root-first bare-name chain:\n{digest}"
        );
        assert!(
            digest.contains(
                "child_item  loc=root_item/child_item  purpose=detail  id=-  action=navigate"
            ),
            "child must lower to its 2-segment bare-name chain:\n{digest}"
        );
        assert!(
            digest.contains("root_item  loc=root_item  purpose=list  id=-  action=leaf"),
            "root must lower to its own single-segment address; no navigates_to/selects_view\
             here so action=leaf, not root:\n{digest}"
        );
    }

    /// Corpus probe (the `[H]->[G]` gate) — env-gated, self-skipping. Harvests
    /// the real `OpenProject` menu tree as `MenuQuad`s, lowers them through
    /// the shared `build_nav_digest`, and proves the `[menu-quad]` section's
    /// `loc=` radix addresses reflect the harvested `part_of` nesting — the
    /// structure-parity twin of the value-parity MySQL/lance-datafusion
    /// reconciler oracle (see `.claude/knowledge/
    /// consumer-transcode-furnace-playbook.md` in the consumer repos).
    ///
    /// Every harvested `(child, parent)` edge is checked, not just the
    /// deepest one: the child's digest `loc` must equal the parent's `loc`
    /// plus the child's own bare name — the address IS the walked rail, at
    /// every depth, not merely at the maximum. `parent_of` conflict
    /// resolution (smallest parent wins, per `part_of_raw`'s sorted-set
    /// `or_insert`) is replayed here exactly as `nav_digest` resolves it
    /// internally, so this probe stays correct even if the corpus ever
    /// reuses an item name across two different menus (`MenuQuad::to_quad`
    /// namespaces on `{ns}:{item}`, not `{ns}:{menu}.{item}`, so that
    /// collision is a real possibility, not a hypothetical).
    ///
    /// A parentless node NEVER resolves to `action=root` here (that action
    /// requires a `navigates_to`/`selects_view` out-edge, which a
    /// menu-quad-only triple set never emits) — every parentless node is
    /// `action=leaf` with a single-segment address equal to its own bare
    /// name. This is asserted explicitly below rather than assumed.
    ///
    /// **Identity-tier note:** `extract_menu_quads` now binds
    /// `identity_concept` (`surfaces_concept`) for CRUD-spine nodes whose
    /// derived `controller -> model` token is roster-verified — see
    /// `derived_identity_matched_binds_open_project_extracted` above. That
    /// does NOT change this probe's expected `loc`/`action` shape:
    /// `ExamConfig::default()` carries an empty `codebook`, so
    /// `resolve_token` never resolves ANY `surfaces_concept` token here
    /// (bound or not) — `classid_of` stays empty and `menu_address`'s bare-
    /// name fallback is exercised exactly as before. A real codebook (Lane
    /// B's `mint_menu_facets`) is what turns a bound identity into a
    /// resolved `id=0x<ID>`, not this structure-only probe.
    #[test]
    #[allow(clippy::print_stderr)] // diagnostic emission gated on env var (real-corpus gate)
    fn corpus_probe_menu_quad_round_trip_over_openproject() {
        let Ok(root) = std::env::var("RAILS_CORPUS_SRC") else {
            eprintln!("RAILS_CORPUS_SRC unset; skipping menu-quad round-trip corpus probe");
            return;
        };
        let path = Path::new(&root);
        let quads = extract_menu_quads(path, "openproject");
        assert!(
            !quads.is_empty(),
            "expected at least one harvested menu quad over the real corpus"
        );

        let triples: Vec<Triple> = quads.iter().flat_map(MenuQuad::to_triples).collect();
        assert!(!triples.is_empty(), "{quads:?}");

        let config = ExamConfig::default();
        let digest = build_nav_digest(&triples, &config);

        // Determinism: the same harvest must lower to a byte-identical
        // digest on a second build (the digest's own shuffle-invariance
        // guarantee, exercised here against the real corpus rather than a
        // hand-built fixture).
        let digest_again = build_nav_digest(&triples, &config);
        assert_eq!(digest, digest_again, "digest must be deterministic");

        // Parse the [menu-quad] section into node -> (loc, action).
        let section = digest
            .split("[menu-quad]\n")
            .nth(1)
            .expect("digest must carry a [menu-quad] section");
        let mut lines: HashMap<String, (String, String)> = HashMap::new();
        for line in section.lines() {
            // "<node>  loc=<addr>  purpose=<p>  id=<id>  action=<action>"
            let mut fields = line.split("  ");
            let Some(node) = fields.next() else { continue };
            let mut loc = String::new();
            let mut action = String::new();
            for field in fields {
                if let Some(v) = field.strip_prefix("loc=") {
                    loc = v.to_string();
                } else if let Some(v) = field.strip_prefix("action=") {
                    action = v.to_string();
                }
            }
            lines.insert(node.to_string(), (loc, action));
        }
        assert!(!lines.is_empty(), "[menu-quad] section must not be empty");

        // Replay nav_digest's own (bare) child -> parent resolution: sorted
        // (child, parent) set, smallest parent wins on conflict.
        let bare = |n: &str| n.split_once(':').map_or(n, |(_, tail)| tail).to_string();
        let mut part_of_raw: BTreeSet<(String, String)> = BTreeSet::new();
        for q in &quads {
            if let Some(parent) = &q.parent {
                part_of_raw.insert((bare(&q.node), bare(parent)));
            }
        }
        assert!(
            !part_of_raw.is_empty(),
            "expected at least one part_of edge in the real menu harvest"
        );
        let mut parent_of: BTreeMap<String, String> = BTreeMap::new();
        for (child, parent) in &part_of_raw {
            parent_of
                .entry(child.clone())
                .or_insert_with(|| parent.clone());
        }

        // Independent radix-walk mirroring `nav_digest::menu_address`'s
        // fallback path (bare node name per ancestor — Rails MenuQuads never
        // bind `identity_concept`, so classid resolution never fires here).
        #[expect(
            clippy::items_after_statements,
            reason = "helper is scoped to this one probe test and reads clearest right where it's used"
        )]
        fn expected_address(node: &str, parent_of: &BTreeMap<String, String>) -> String {
            let mut chain = Vec::new();
            let mut seen = BTreeSet::new();
            let mut cur = node.to_string();
            loop {
                if !seen.insert(cur.clone()) || chain.len() >= 32 {
                    break;
                }
                chain.push(cur.clone());
                match parent_of.get(&cur) {
                    Some(p) => cur = p.clone(),
                    None => break,
                }
            }
            chain.reverse();
            chain.join("/")
        }

        // Structure-parity check: EVERY harvested (child, parent) edge, not
        // just the deepest chain — the digest's loc must equal the
        // independently-walked rail, and must carry the parent's own loc as
        // a root-first prefix.
        let mut max_depth = 1usize;
        for (child, parent) in &parent_of {
            let (child_loc, child_action) = lines.get(child).unwrap_or_else(|| {
                panic!("no [menu-quad] digest line for harvested node {child:?}")
            });
            assert_eq!(
                child_action, "navigate",
                "{child} has a part_of parent, so its digest action must be navigate"
            );
            let expected = expected_address(child, &parent_of);
            assert_eq!(
                *child_loc, expected,
                "loc address for {child} must equal the walked part_of rail"
            );
            max_depth = max_depth.max(expected.split('/').count());

            if let Some((parent_loc, _)) = lines.get(parent) {
                assert!(
                    child_loc.starts_with(parent_loc.as_str()),
                    "{child}'s address {child_loc} must carry its parent {parent}'s address \
                     {parent_loc} as a root-first prefix"
                );
            }
        }
        assert!(
            max_depth >= 2,
            "expected at least one nested (depth >= 2) menu-quad chain in the real corpus"
        );

        // Every parentless node's address is exactly its own bare name (the
        // depth-1 base case of the root-first path property), and its
        // action is `leaf` — never `root`, since this triple set carries no
        // navigates_to/selects_view out-edges for `menu_roots` to key off.
        let mut parentless_checked = 0usize;
        #[expect(
            clippy::iter_over_hash_type,
            reason = "each entry is checked independently against only its own \
                value and a running count; the assertions below do not depend \
                on iteration order"
        )]
        for (node, (loc, action)) in &lines {
            if !parent_of.contains_key(node) {
                assert_eq!(
                    loc, node,
                    "a parentless node's address must be its own bare name"
                );
                assert_eq!(
                    action, "leaf",
                    "a parentless node in a menu-quad-only triple set must be action=leaf"
                );
                parentless_checked += 1;
            }
        }
        assert!(
            parentless_checked > 0,
            "expected at least one parentless menu-quad node"
        );

        eprintln!(
            "menu-quad round-trip corpus probe: {} quads, {} triples, {} part_of edges, max depth {}",
            quads.len(),
            triples.len(),
            parent_of.len(),
            max_depth,
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5 — the `if:`-guard permission arm (OQ-GUARD-1)
    // ─────────────────────────────────────────────────────────────────────

    fn guard_entry(case: &str, guard_src: &str) -> RegionEntry {
        let root = scratch_dir(case);
        write_file(
            &root,
            "config/initializers/menus.rb",
            &format!(
                "Redmine::MenuManager.map :top_menu do |menu|\n\
                 \x20 menu.push :x, {{ controller: \"/x\" }}, if: {guard_src}\n\
                 end\n"
            ),
        );
        let entries = extract_regions(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(entries.len(), 1, "{entries:?}");
        entries.into_iter().next().expect("one entry")
    }

    /// A single `allowed_to?(:sym)` → one symbol, and `to_guard_triples`
    /// emits the bare-node `guarded_by_permission` fact at Inferred.
    #[test]
    fn guard_single_permission_extracted() {
        let entry = guard_entry(
            "guard_single",
            "->(_) { User.current.allowed_to?(:view_foo) }",
        );
        assert_eq!(entry.permissions, vec!["view_foo".to_string()]);

        let triples = entry.to_guard_triples("openproject");
        assert_eq!(triples.len(), 1, "{triples:?}");
        let t = &triples[0];
        assert_eq!(t.s, "openproject:x");
        assert_eq!(t.p, "guarded_by_permission");
        assert_eq!(t.o, "view_foo");
        // Inferred tier — a proc-body heuristic.
        assert_eq!((t.f, t.c), Provenance::Inferred.truth());
    }

    /// A conjunction (`allowed_to?(:a) && allowed_in_project?(:b)`) → BOTH
    /// symbols (sorted).
    #[test]
    fn guard_conjunction_both_symbols() {
        let entry = guard_entry(
            "guard_conjunction",
            "->(_) { User.current.allowed_to?(:view_a) && User.current.allowed_in_project?(:manage_b) }",
        );
        assert_eq!(
            entry.permissions,
            vec!["manage_b".to_string(), "view_a".to_string()]
        );
    }

    /// A disjunction (`allowed_globally?(:add_project) ||
    /// allowed_in_project?(:add_subprojects, project)`) → BOTH symbols
    /// emitted. This is the honest `guarded_by` semantics: both permissions
    /// appear in the visibility guard, so a flat "requires" would misencode
    /// it. (The `allowed_in_project?(:sym, project)` shape keeps `:sym` FIRST
    /// — the `project` receiver-context arg trails it, so this is normal-form,
    /// not receiver-style.)
    #[test]
    fn guard_disjunction_both_symbols() {
        let entry = guard_entry(
            "guard_disjunction",
            "->(_) { User.current.allowed_globally?(:add_project) || User.current.allowed_in_project?(:add_subprojects, project) }",
        );
        assert_eq!(
            entry.permissions,
            vec!["add_project".to_string(), "add_subprojects".to_string()]
        );
    }

    /// A receiver-style `allowed_to(User.current, :view_x)` — the permission
    /// is the SECOND positional (the first is a user-expression) → `["view_x"]`.
    #[test]
    fn guard_receiver_style_extracted() {
        let entry = guard_entry(
            "guard_receiver_style",
            "->(_) { allowed_to(User.current, :view_x) }",
        );
        assert_eq!(entry.permissions, vec!["view_x".to_string()]);
    }

    /// A no-permission guard (`if: ->(_) { admin? }`) → no symbol, no guard
    /// triple.
    #[test]
    fn guard_no_permission_yields_empty() {
        let entry = guard_entry("guard_none", "->(_) { admin? }");
        assert!(entry.permissions.is_empty(), "{:?}", entry.permissions);
        assert!(entry.to_guard_triples("openproject").is_empty());
    }

    /// Env-gated real-corpus probe (self-skips without `RAILS_CORPUS_SRC`).
    /// The OQ-GUARD-1 measurement over `OpenProject` found ~10 permission-
    /// bearing menu items (single 2 + disjunction 1 + mixed 7; the 2 dynamic
    /// guards contribute no symbol) spanning 11 distinct permission symbols.
    /// Asserted as firm lower bounds; the eprintln prints the exact measured
    /// values so the bounds can be tightened at first green.
    #[test]
    #[allow(clippy::print_stderr)] // diagnostic emission gated on env var (real-corpus gate)
    fn corpus_probe_guard_arm_over_openproject() {
        let Ok(root) = std::env::var("RAILS_CORPUS_SRC") else {
            eprintln!("RAILS_CORPUS_SRC unset; skipping guard-arm corpus probe");
            return;
        };
        let path = Path::new(&root);
        let (entries, report) = extract_regions_with_report(path, "openproject");
        assert!(report.with_permission >= 10, "{report:?}");

        let triples = extract_menu_guards(path, "openproject");
        assert!(!triples.is_empty(), "{report:?}");
        // Every emitted guard triple carries the closed-vocab predicate at the
        // Inferred tier and a bare-node subject.
        for t in &triples {
            assert_eq!(t.p, "guarded_by_permission");
            assert_eq!((t.f, t.c), Provenance::Inferred.truth());
            assert!(t.s.starts_with("openproject:"), "{t:?}");
        }

        let distinct: BTreeSet<&str> = triples.iter().map(|t| t.o.as_str()).collect();
        assert!(distinct.len() >= 10, "distinct symbols: {distinct:?}");

        eprintln!(
            "guard-arm corpus probe: {} items, {} with_permission, {} guard triples, {} distinct symbols",
            entries.len(),
            report.with_permission,
            triples.len(),
            distinct.len(),
        );
    }
}
