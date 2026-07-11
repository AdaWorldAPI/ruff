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
use ruff_spo_triplet::{Predicate, Provenance, Triple};

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
    /// The resolved 0-based sibling ordinal (§3). `None` means the item's
    /// sibling group had a `before`/`after` cycle — genuinely unresolvable,
    /// not a guess (counted in [`RegionScanReport::unresolved_order`]).
    pub tab_order: Option<u32>,
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
    /// Items whose sibling group had a `before`/`after` cycle — no
    /// `tab_order` was assigned for them (never a guess).
    pub unresolved_order: usize,
    /// Distinct `menu_names` seen, sorted.
    pub menus: Vec<String>,
}

/// One collected `menu.push` site, pre-tab_order-resolution. Frontend-local
/// — never exposed; [`extract_regions_with_report`] converts each into a
/// [`RegionEntry`] once §3's two-pass resolution has run.
struct Registration {
    menu: String,
    item: String,
    parent: Option<String>,
    position: Position,
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
            file: r.file,
        })
        .collect();

    report.items = entries.len();
    report.with_parent = entries.iter().filter(|e| e.parent.is_some()).count();
    report.with_position = entries
        .iter()
        .filter(|e| !matches!(e.position, Position::Append))
        .count();
    let mut menus: Vec<String> = entries.iter().map(|e| e.menu.clone()).collect();
    menus.sort();
    menus.dedup();
    report.menus = menus;

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

/// Resolve `tab_order` for every registration, per §3 of the frozen spec:
/// group by `(menu, parent)` sibling group, seed with declaration order
/// (the `regs` slice's own index order — files are walked in sorted-path
/// order and each file's AST is visited in source order, so index order
/// IS the "file+line declaration order" approximation the spec names as
/// its one ordering assumption), then apply First / Last / Before / After
/// in that fixed sequence. Returns one `Option<u32>` per `regs` entry (by
/// index) plus the total count of cyclic (unresolvable) entries.
fn resolve_tab_orders(regs: &[Registration]) -> (Vec<Option<u32>>, usize) {
    let mut tab_order: Vec<Option<u32>> = vec![None; regs.len()];
    let mut unresolved = 0usize;

    let mut groups: HashMap<(String, Option<String>), Vec<usize>> = HashMap::new();
    for (i, r) in regs.iter().enumerate() {
        groups
            .entry((r.menu.clone(), r.parent.clone()))
            .or_default()
            .push(i);
    }

    for order in groups.into_values() {
        resolve_group(regs, order, &mut tab_order, &mut unresolved);
    }

    (tab_order, unresolved)
}

/// Resolve one `(menu, parent)` sibling group in place, per the 5-step
/// algorithm (§3): cycle-detect first (functional graph, out-degree <= 1
/// per node since each registration carries exactly one [`Position`]),
/// then First → Last → Before/After → assign ordinals.
fn resolve_group(
    regs: &[Registration],
    order: Vec<usize>,
    tab_order: &mut [Option<u32>],
    unresolved: &mut usize,
) {
    let item_index: HashMap<&str, usize> =
        order.iter().map(|&i| (regs[i].item.as_str(), i)).collect();
    let mut edge: HashMap<usize, usize> = HashMap::new();
    for &i in &order {
        let anchor = match &regs[i].position {
            Position::Before(a) | Position::After(a) => Some(a.as_str()),
            _ => None,
        };
        if let Some(a) = anchor
            && let Some(&target) = item_index.get(a)
            && target != i
        {
            edge.insert(i, target);
        }
    }
    let cyclic = detect_cycle_members(&order, &edge);

    // Step 2: First items move to the front, relative order preserved.
    let (firsts, rest): (Vec<usize>, Vec<usize>) = order
        .into_iter()
        .partition(|&i| matches!(regs[i].position, Position::First));
    let mut seq: Vec<usize> = firsts;
    seq.extend(rest);

    // Step 3: Last items move to the end, relative order preserved.
    let (rest2, lasts): (Vec<usize>, Vec<usize>) = seq
        .into_iter()
        .partition(|&i| !matches!(regs[i].position, Position::Last));
    let mut seq: Vec<usize> = rest2;
    seq.extend(lasts);

    // Step 4: Before/After, resolved in declaration order (the stable
    // partitions above keep the untouched middle band declaration-ordered,
    // so filtering `seq` here yields Before/After items in original seq
    // order too). Cyclic items are skipped — they keep their step-2/3
    // position and get no tab_order (step 5).
    let before_after: Vec<usize> = seq
        .iter()
        .copied()
        .filter(|i| {
            !cyclic.contains(i)
                && matches!(regs[*i].position, Position::Before(_) | Position::After(_))
        })
        .collect();
    for id in before_after {
        let Some(cur) = seq.iter().position(|&x| x == id) else {
            continue;
        };
        seq.remove(cur);
        let (anchor, is_before) = match &regs[id].position {
            Position::Before(a) => (a.as_str(), true),
            Position::After(a) => (a.as_str(), false),
            _ => unreachable!("filtered to Before/After above"),
        };
        match seq.iter().position(|&x| regs[x].item.as_str() == anchor) {
            Some(anchor_idx) => {
                let insert_at = if is_before {
                    anchor_idx
                } else {
                    anchor_idx + 1
                };
                seq.insert(insert_at, id);
            }
            // Missing anchor -> Rails' own documented missing-anchor
            // behavior (mapper.rb): fall back to append.
            None => seq.push(id),
        }
    }

    // Step 5: assign 0-based indices; cyclic items get no tab_order.
    for (idx, id) in seq.into_iter().enumerate() {
        if cyclic.contains(&id) {
            *unresolved += 1;
        } else {
            tab_order[id] = Some(u32::try_from(idx).unwrap_or(u32::MAX));
        }
    }
}

/// Find every item that participates in a `before`/`after` cycle within
/// one sibling group. `edge` is a functional graph (out-degree <= 1 per
/// node, since each registration carries exactly one [`Position`]), so
/// this is a plain follow-the-chain walk with a three-color visited set
/// (0 = unvisited, 1 = in the current path, 2 = done/resolved) rather than
/// general-graph Tarjan/Kosaraju machinery.
fn detect_cycle_members(order: &[usize], edge: &HashMap<usize, usize>) -> HashSet<usize> {
    let mut cyclic = HashSet::new();
    let mut state: HashMap<usize, u8> = HashMap::new();
    for &start in order {
        if state.get(&start).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut cur = start;
        loop {
            match state.get(&cur).copied().unwrap_or(0) {
                0 => {
                    state.insert(cur, 1);
                    path.push(cur);
                    match edge.get(&cur) {
                        Some(&next) => cur = next,
                        None => break,
                    }
                }
                1 => {
                    if let Some(pos) = path.iter().position(|&n| n == cur) {
                        for &n in &path[pos..] {
                            cyclic.insert(n);
                        }
                    }
                    break;
                }
                _ => break,
            }
        }
        for &n in &path {
            state.insert(n, 2);
        }
    }
    cyclic
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
    w.regs.push(Registration {
        menu: menu.to_string(),
        item,
        parent,
        position,
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

impl RegionEntry {
    /// Lift into the shared closed-vocab triples (§1/§2 of the frozen
    /// spec): `docked_at` always, `tab_order` when resolved, and
    /// `contains_control` when `parent` is present. All
    /// [`Provenance::Authoritative`], matching ruff #76's `WinForms` arm.
    #[must_use]
    pub fn to_triples(&self, namespace: &str) -> Vec<Triple> {
        let subject = format!("{namespace}:{}.{}", self.menu, self.item);
        let mut triples = vec![Triple::new(
            subject.clone(),
            Predicate::DockedAt,
            self.menu.clone(),
            Provenance::Authoritative,
        )];
        if let Some(order) = self.tab_order {
            triples.push(Triple::new(
                subject.clone(),
                Predicate::TabOrder,
                order.to_string(),
                Provenance::Authoritative,
            ));
        }
        if let Some(parent) = &self.parent {
            let parent_subject = format!("{namespace}:{}.{parent}", self.menu);
            triples.push(Triple::new(
                parent_subject,
                Predicate::ContainsControl,
                subject,
                Provenance::Authoritative,
            ));
        }
        triples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Fixture (e) — `after:` with a MISSING anchor → append fallback
    /// (Rails' own documented missing-anchor behavior).
    #[test]
    fn after_missing_anchor_falls_back_to_append() {
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
        assert_eq!(order("gamma"), Some(1));
        assert_eq!(order("beta"), Some(2));

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

    /// Fixture (i) — a before/after cycle → `unresolved_order`, no
    /// `tab_order` for the affected items (never a guess).
    #[test]
    fn before_after_cycle_yields_no_tab_order_and_counts_unresolved() {
        let root = scratch_dir("cycle");
        write_file(
            &root,
            "config/initializers/menus.rb",
            "Redmine::MenuManager.map :top_menu do |menu|\n\
             \x20 menu.push :alpha, { controller: \"/a\" }, after: :beta\n\
             \x20 menu.push :beta, { controller: \"/b\" }, after: :alpha\n\
             end\n",
        );

        let (entries, report) = extract_regions_with_report(&root, "openproject");
        assert_eq!(report.unresolved_order, 2, "{report:?}");
        for e in &entries {
            assert!(e.tab_order.is_none(), "{e:?}");
        }

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

        eprintln!(
            "region-arm corpus probe: {} files, {} map_blocks, {} items, {} with_parent, {} unresolved",
            report.files_scanned,
            report.map_blocks,
            report.items,
            report.with_parent,
            report.unresolved_order,
        );
    }
}
